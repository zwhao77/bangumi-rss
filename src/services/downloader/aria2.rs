//! Aria2 downloader — JSON-RPC client behind the `TorrentDownloader` trait.
//!
//! Stateless: every infohash → gid lookup rebuilds the mapping from
//! `tellActive` + `tellStopped`.  Two extra RPC calls per `HandleCompleted`
//! (~10-15 ms) are negligible at the download-completion rate.

use std::collections::HashMap;

use crate::traits::{OpResult, TorrentDownloader};
use crate::types::{CompletedDownload, DownloadSnapshot, TorrentFile};

/// Concrete downloader backed by aria2's JSON-RPC API.
///
/// Holds no mutable state — safe to share via `Arc`.
pub struct Aria2Downloader {
    rpc_url: String,
    secret: String,
}

impl Aria2Downloader {
    pub fn with_rpc_url(rpc_url: String, secret: String) -> Self {
        Self { rpc_url, secret }
    }

    // ── Low-level JSON-RPC ──

    fn rpc(&self, method: &str, params: &[serde_json::Value]) -> Option<serde_json::Value> {
        // Prepend token if configured (aria2 --rpc-secret).
        let params = if self.secret.is_empty() {
            params.to_vec()
        } else {
            let mut p = vec![serde_json::json!(format!("token:{}", self.secret))];
            p.extend_from_slice(params);
            p
        };
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "id": "1",
            "method": format!("aria2.{method}"),
            "params": params,
        });
        let resp = ureq::post(&self.rpc_url)
            .timeout(std::time::Duration::from_secs(
                crate::config::HTTP_TIMEOUT_SECS,
            ))
            .send_json(payload)
            .ok()?;
        let body: serde_json::Value = resp.into_json().ok()?;
        if body.get("error").is_some() {
            return None;
        }
        Some(body["result"].clone())
    }

    // ── GID resolution (stateless, paginated) ──

    /// Fetch infohash → gid mapping for a slice of stopped tasks.
    ///
    /// `offset` is 0-based.  Returns an empty map when there are no more tasks.
    fn fetch_gid_map_range(&self, offset: i32, limit: i32) -> HashMap<String, String> {
        self.rpc(
            "tellStopped",
            &[
                serde_json::json!(offset),
                serde_json::json!(limit),
                serde_json::json!(["gid", "infoHash"]),
            ],
        )
        .and_then(|r| r.as_array().cloned())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|t| {
            let ih = t["infoHash"].as_str()?;
            let gid = t["gid"].as_str()?;
            Some((ih.to_string(), gid.to_string()))
        })
        .collect()
    }

    /// Resolve infohash → gid.
    ///
    /// Checks active tasks first (1 RPC), then paginates through stopped
    /// tasks until the infohash is found or no more pages exist.
    fn with_gid(&self, infohash: &str) -> anyhow::Result<String> {
        // 1. Check active tasks — usually 0-10 items, single RPC.
        let active: Vec<_> = self
            .rpc("tellActive", &[serde_json::json!(["gid", "infoHash"])])
            .and_then(|r| r.as_array().cloned())
            .unwrap_or_default()
            .into_iter()
            .filter_map(|t| {
                let ih = t["infoHash"].as_str()?;
                let gid = t["gid"].as_str()?;
                Some((ih.to_string(), gid.to_string()))
            })
            .collect();
        for (ih, gid) in &active {
            if ih == infohash {
                return Ok(gid.clone());
            }
        }

        // 2. Paginate through stopped tasks.
        let page: i32 = 1000;
        let mut offset: i32 = 0;
        loop {
            let map = self.fetch_gid_map_range(offset, page);
            if map.is_empty() {
                break;
            }
            if let Some(gid) = map.get(infohash) {
                return Ok(gid.clone());
            }
            if (map.len() as i32) < page {
                break; // last page
            }
            offset += page;
        }

        Err(anyhow::anyhow!("task not found in aria2: {infohash}"))
    }
}

impl TorrentDownloader for Aria2Downloader {
    fn add_uri(&self, uri: &str, dir: &str) -> anyhow::Result<String> {
        let result = self.rpc(
            "addUri",
            &[serde_json::json!([uri]), serde_json::json!({ "dir": dir })],
        );
        let gid = result
            .and_then(|r| r.as_str().map(String::from))
            .ok_or_else(|| anyhow::anyhow!("aria2.addUri returned no GID"))?;

        let infohash = (0..30)
            .find_map(|i| {
                if i > 0 {
                    std::thread::sleep(std::time::Duration::from_secs(1));
                }
                self.rpc(
                    "tellStatus",
                    &[serde_json::json!(gid), serde_json::json!(["infoHash"])],
                )
                .and_then(|s| s["infoHash"].as_str().map(String::from))
                .filter(|s| !s.is_empty())
            })
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "aria2.addUri succeeded (gid={gid}) but tellStatus returned no infoHash after 30 retries"
                )
            })?;

        Ok(infohash)
    }

    fn add_torrent_bytes(&self, data: &[u8], dir: &str) -> anyhow::Result<String> {
        let b64 = {
            use base64::Engine;
            base64::engine::general_purpose::STANDARD.encode(data)
        };
        let result = self.rpc(
            "addTorrent",
            &[
                serde_json::json!(b64),
                serde_json::json!([]),
                serde_json::json!({ "dir": dir }),
            ],
        );
        let gid = result
            .and_then(|r| r.as_str().map(String::from))
            .ok_or_else(|| anyhow::anyhow!("aria2.addTorrent returned no GID"))?;

        let infohash = self
            .rpc(
                "tellStatus",
                &[serde_json::json!(gid), serde_json::json!(["infoHash"])],
            )
            .and_then(|s| s["infoHash"].as_str().map(String::from))
            .unwrap_or_default();

        Ok(infohash)
    }

    fn list_files(&self, infohash: &str) -> anyhow::Result<Vec<TorrentFile>> {
        let gid = self.with_gid(infohash)?;
        let result = self.rpc("getFiles", &[serde_json::json!(gid)]);
        let arr = result
            .and_then(|r| r.as_array().cloned())
            .unwrap_or_default();
        Ok(arr
            .iter()
            .filter_map(|f| {
                let path = f["path"].as_str()?.to_string();
                let name = std::path::Path::new(&path)
                    .file_name()?
                    .to_string_lossy()
                    .to_string();
                Some(TorrentFile { path, name })
            })
            .collect())
    }

    fn rename_file(
        &self,
        infohash: &str,
        _old_path: &str,
        _new_name: &str,
    ) -> anyhow::Result<OpResult> {
        // aria2's changeOption("out") only works for single-file HTTP downloads.
        // For BitTorrent multi-file downloads there is no rename API.
        // Returning Unsupported signals the caller to fall back to filesystem rename.
        let _ = infohash;
        Ok(OpResult::Unsupported)
    }

    fn move_files(&self, _infohash: &str, _new_location: &str) -> anyhow::Result<OpResult> {
        // aria2 has no built-in move-directory command.
        Ok(OpResult::Unsupported)
    }

    fn pause(&self, infohash: &str) -> anyhow::Result<()> {
        let gid = self.with_gid(infohash)?;
        self.rpc("forcePause", &[serde_json::json!(gid)])
            .ok_or_else(|| anyhow::anyhow!("aria2: pause failed for {infohash}"))?;
        Ok(())
    }

    fn resume(&self, infohash: &str) -> anyhow::Result<()> {
        let gid = self.with_gid(infohash)?;
        self.rpc("unpause", &[serde_json::json!(gid)])
            .ok_or_else(|| anyhow::anyhow!("aria2: resume failed for {infohash}"))?;
        Ok(())
    }

    fn remove(&self, infohash: &str, _delete_files: bool) -> anyhow::Result<()> {
        let gid = self.with_gid(infohash)?;
        self.rpc("remove", &[serde_json::json!(gid)])
            .ok_or_else(|| anyhow::anyhow!("aria2: remove failed for {infohash}"))?;
        self.rpc("removeDownloadResult", &[serde_json::json!(gid)])
            .ok_or_else(|| anyhow::anyhow!("aria2: removeDownloadResult failed for {infohash}"))?;
        Ok(())
    }

    fn poll_completed(&self) -> anyhow::Result<Vec<CompletedDownload>> {
        // Query stopped tasks with status "complete".
        let stopped = self.rpc(
            "tellStopped",
            &[
                serde_json::json!(-1),
                serde_json::json!(1000),
                serde_json::json!(["gid", "infoHash", "status"]),
            ],
        );
        // Also query active tasks — those at 100% are seeding (= done).
        let active = self.rpc(
            "tellActive",
            &[serde_json::json!([
                "gid",
                "infoHash",
                "status",
                "totalLength",
                "completedLength"
            ])],
        );

        let mut completed = Vec::new();

        // Stopped + complete.
        for t in stopped
            .as_ref()
            .and_then(|r| r.as_array())
            .cloned()
            .unwrap_or_default()
        {
            if t["status"].as_str() != Some("complete") {
                continue;
            }
            if let (Some(ih), _gid) = (t["infoHash"].as_str(), t["gid"].as_str()) {
                completed.push(CompletedDownload {
                    infohash: ih.to_string(),
                });
            }
        }

        // Active + 100% = seeding → treat as completed.
        for t in active
            .as_ref()
            .and_then(|r| r.as_array())
            .cloned()
            .unwrap_or_default()
        {
            let total: u64 = t["totalLength"]
                .as_str()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            let done: u64 = t["completedLength"]
                .as_str()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            if total == 0 || done < total {
                continue;
            }
            if let (Some(ih), _gid) = (t["infoHash"].as_str(), t["gid"].as_str()) {
                completed.push(CompletedDownload {
                    infohash: ih.to_string(),
                });
            }
        }

        Ok(completed)
    }

    fn poll_failed(&self) -> anyhow::Result<Vec<CompletedDownload>> {
        let result = self.rpc(
            "tellStopped",
            &[
                serde_json::json!(-1),
                serde_json::json!(1000),
                serde_json::json!(["gid", "infoHash", "status"]),
            ],
        );
        let arr = result
            .and_then(|r| r.as_array().cloned())
            .unwrap_or_default();

        let mut failed = Vec::new();
        for t in &arr {
            if t["status"].as_str() != Some("error") {
                continue;
            }
            if let (Some(ih), _gid) = (t["infoHash"].as_str(), t["gid"].as_str()) {
                failed.push(CompletedDownload {
                    infohash: ih.to_string(),
                });
            }
        }
        Ok(failed)
    }

    fn query_all(&self) -> anyhow::Result<Vec<DownloadSnapshot>> {
        let mut snapshots = Vec::new();

        let tasks = self
            .rpc(
                "tellActive",
                &[serde_json::json!([
                    "gid",
                    "infoHash",
                    "status",
                    "totalLength",
                    "completedLength",
                    "downloadSpeed",
                    "bittorrent"
                ])],
            )
            .and_then(|r| r.as_array().cloned())
            .unwrap_or_default()
            .into_iter()
            .chain(
                self.rpc(
                    "tellStopped",
                    &[
                        serde_json::json!(-1),
                        serde_json::json!(1000),
                        serde_json::json!([
                            "gid",
                            "infoHash",
                            "status",
                            "totalLength",
                            "completedLength",
                            "downloadSpeed",
                            "bittorrent"
                        ]),
                    ],
                )
                .and_then(|r| r.as_array().cloned())
                .unwrap_or_default(),
            );

        for t in tasks {
            let infohash = t["infoHash"].as_str().unwrap_or("").to_string();
            if infohash.is_empty() {
                continue;
            }
            let gid = t["gid"].as_str().unwrap_or("");
            let total: u64 = t["totalLength"]
                .as_str()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            let done: u64 = t["completedLength"]
                .as_str()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            let progress = if total > 0 {
                done as f32 / total as f32
            } else {
                0.0
            };
            let state = match t["status"].as_str().unwrap_or("") {
                "complete" => crate::types::DownloadState::Completed,
                "active" if done >= total && total > 0 => crate::types::DownloadState::Seeding,
                "active" => crate::types::DownloadState::Downloading,
                "waiting" => crate::types::DownloadState::Waiting,
                "paused" => crate::types::DownloadState::Paused,
                "error" => crate::types::DownloadState::Failed,
                "removed" => crate::types::DownloadState::Removed,
                _ => continue,
            };
            let speed: u64 = t["downloadSpeed"]
                .as_str()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            let name = t["bittorrent"]["info"]["name"]
                .as_str()
                .unwrap_or(gid)
                .to_string();

            snapshots.push(DownloadSnapshot {
                infohash,
                state,
                progress,
                speed,
                size: total,
                name,
            });
        }
        Ok(snapshots)
    }

    fn check_connection(&self) -> anyhow::Result<()> {
        self.rpc("getVersion", &[])
            .ok_or_else(|| anyhow::anyhow!("aria2 not reachable or authentication failed"))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Unit tests ──

    #[test]
    fn with_gid_miss_returns_error() {
        let dl = Aria2Downloader {
            rpc_url: String::new(),
            secret: String::new(),
        };
        let result = dl.with_gid("deadbeef");
        assert!(result.is_err());
    }
}
