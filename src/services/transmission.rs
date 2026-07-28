//! Transmission downloader — JSON-RPC 2.0 client behind `TorrentDownloader`.
//!
//! Transmission 4.1.0+ uses JSON-RPC 2.0 (snake_case methods).
//! CSRF protection: first request returns HTTP 409 with `X-Transmission-Session-Id`;
//! subsequent requests include that header.

use std::sync::Mutex;

use base64::Engine;
use crate::traits::TorrentDownloader;
use crate::types::{CompletedDownload, DownloadSnapshot, DownloadState, TorrentFile};

/// Concrete downloader backed by Transmission's RPC API.
pub struct TransmissionDownloader {
    rpc_url: String,
    username: String,
    password: String,
    /// Cached CSRF session id.  Refreshed on 409 responses.
    session_id: Mutex<Option<String>>,
}

impl TransmissionDownloader {
    pub fn with_rpc_url(rpc_url: String, username: String, password: String) -> Self {
        Self {
            rpc_url: rpc_url.trim_end_matches('/').to_string(),
            username,
            password,
            session_id: Mutex::new(None),
        }
    }

    // ── Low-level JSON-RPC 2.0 ──

    fn rpc(&self, method: &str, params: &serde_json::Value) -> Option<serde_json::Value> {
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        });
        let payload_str = serde_json::to_string(&payload).ok()?;

        let req = ureq::post(&self.rpc_url)
            .set("Content-Type", "application/json")
            .timeout(std::time::Duration::from_secs(
                crate::config::HTTP_TIMEOUT_SECS,
            ));

        // Add auth if configured.
        let req = if !self.username.is_empty() {
            let auth = format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD
                    .encode(format!("{}:{}", self.username, self.password))
            );
            req.set("Authorization", &auth)
        } else {
            req
        };

        // Add CSRF session id if we have one.
        let req = if let Some(ref sid) = *self.session_id.lock().unwrap() {
            req.set("X-Transmission-Session-Id", sid)
        } else {
            req
        };

        let resp = req.send_string(&payload_str).ok()?;

        // Handle CSRF 409: extract session id from headers and retry once.
        if resp.status() == 409 {
            let new_sid = resp
                .header("X-Transmission-Session-Id")
                .map(String::from)?;
            *self.session_id.lock().unwrap() = Some(new_sid.clone());

            let retry_req = ureq::post(&self.rpc_url)
                .set("Content-Type", "application/json")
                .set("X-Transmission-Session-Id", &new_sid)
                .timeout(std::time::Duration::from_secs(
                    crate::config::HTTP_TIMEOUT_SECS,
                ));
            let retry_req = if !self.username.is_empty() {
                let auth = format!(
                    "Basic {}",
                    base64::engine::general_purpose::STANDARD
                        .encode(format!("{}:{}", self.username, self.password))
                );
                retry_req.set("Authorization", &auth)
            } else {
                retry_req
            };
            let retry_resp = retry_req.send_string(&payload_str).ok()?;
            return retry_resp.into_json().ok();
        }

        let body: serde_json::Value = resp.into_json().ok()?;
        if body.get("error").is_some() {
            return None;
        }
        // JSON-RPC 2.0: result is directly under "result" key.
        Some(body["result"].clone())
    }

}

impl TorrentDownloader for TransmissionDownloader {
    fn add_uri(&self, uri: &str, dir: &str) -> anyhow::Result<String> {
        let result = self.rpc(
            "torrent_add",
            &serde_json::json!({
                "filename": uri,
                "download_dir": dir,
                "paused": false,
            }),
        );
        let added = result
            .as_ref()
            .and_then(|r| r["torrent_added"].as_object())
            .ok_or_else(|| anyhow::anyhow!("transmission: torrent_add returned no result"))?;

        added["hash_string"]
            .as_str()
            .map(String::from)
            .ok_or_else(|| anyhow::anyhow!("transmission: no hash_string in torrent_add response"))
    }

    fn add_torrent_bytes(&self, data: &[u8], dir: &str) -> anyhow::Result<String> {
        let b64 = base64::engine::general_purpose::STANDARD.encode(data);
        let result = self.rpc(
            "torrent_add",
            &serde_json::json!({
                "metainfo": b64,
                "download_dir": dir,
                "paused": false,
            }),
        );
        let added = result
            .as_ref()
            .and_then(|r| r["torrent_added"].as_object())
            .or_else(|| {
                // Duplicate torrent: check for torrent_duplicate key.
                result
                    .as_ref()
                    .and_then(|r| r["torrent_duplicate"].as_object())
            })
            .ok_or_else(|| anyhow::anyhow!("transmission: torrent_add (bytes) returned no result"))?;

        added["hash_string"]
            .as_str()
            .map(String::from)
            .ok_or_else(|| anyhow::anyhow!("transmission: no hash_string in add response"))
    }

    fn list_files(&self, infohash: &str) -> anyhow::Result<Vec<TorrentFile>> {
        let result = self.rpc(
            "torrent_get",
            &serde_json::json!({
                "fields": ["id", "files", "file_stats"],
                "ids": [infohash],
            }),
        );
        let torrents = result
            .as_ref()
            .and_then(|r| r["torrents"].as_array())
            .ok_or_else(|| anyhow::anyhow!("transmission: torrent_get returned no torrents"))?;

        let torrent = torrents
            .first()
            .ok_or_else(|| anyhow::anyhow!("transmission: torrent not found: {infohash}"))?;

        let files = torrent["files"]
            .as_array()
            .cloned()
            .unwrap_or_default();

        Ok(files
            .iter()
            .map(|f| {
                let path = f["name"].as_str().unwrap_or("").to_string();
                let name = std::path::Path::new(&path)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                TorrentFile { path, name }
            })
            .collect())
    }

    fn rename_file(
        &self,
        infohash: &str,
        old_path: &str,
        new_name: &str,
    ) -> anyhow::Result<bool> {
        let result = self.rpc(
            "torrent_rename_path",
            &serde_json::json!({
                "ids": [infohash],
                "path": old_path,
                "name": new_name,
            }),
        );
        Ok(result.is_some())
    }

    fn move_files(&self, infohash: &str, new_location: &str) -> anyhow::Result<bool> {
        let result = self.rpc(
            "torrent_set_location",
            &serde_json::json!({
                "ids": [infohash],
                "location": new_location,
                "move": true,
            }),
        );
        Ok(result.is_some())
    }

    fn pause(&self, infohash: &str) -> anyhow::Result<bool> {
        let result = self.rpc(
            "torrent_stop",
            &serde_json::json!({
                "ids": [infohash],
            }),
        );
        Ok(result.is_some())
    }

    fn remove(&self, infohash: &str, delete_files: bool) -> anyhow::Result<bool> {
        let result = self.rpc(
            "torrent_remove",
            &serde_json::json!({
                "ids": [infohash],
                "delete_local_data": delete_files,
            }),
        );
        Ok(result.is_some())
    }

    fn poll_completed(&self) -> anyhow::Result<Vec<CompletedDownload>> {
        let result = self.rpc(
            "torrent_get",
            &serde_json::json!({
                "fields": ["id", "hash_string", "status", "percent_done", "error"],
            }),
        );
        let torrents = result
            .as_ref()
            .and_then(|r| r["torrents"].as_array())
            .cloned()
            .unwrap_or_default();

        let mut completed = Vec::new();
        for t in &torrents {
            let status = t["status"].as_i64().unwrap_or(-1);
            let percent: f64 = t["percent_done"].as_f64().unwrap_or(0.0);
            // Status 0 = stopped, 6 = seeding.  Both mean download is complete.
            if (status == 0 || status == 6) && percent >= 1.0 {
                if let Some(hash) = t["hash_string"].as_str() {
                    completed.push(CompletedDownload {
                        infohash: hash.to_string(),
                    });
                }
            }
        }
        Ok(completed)
    }

    fn poll_failed(&self) -> anyhow::Result<Vec<CompletedDownload>> {
        let result = self.rpc(
            "torrent_get",
            &serde_json::json!({
                "fields": ["id", "hash_string", "status", "error", "error_string"],
            }),
        );
        let torrents = result
            .as_ref()
            .and_then(|r| r["torrents"].as_array())
            .cloned()
            .unwrap_or_default();

        let mut failed = Vec::new();
        for t in &torrents {
            let status = t["status"].as_i64().unwrap_or(-1);
            let error = t["error"].as_i64().unwrap_or(0);
            if status == 0 && error != 0 {
                if let Some(hash) = t["hash_string"].as_str() {
                    failed.push(CompletedDownload {
                        infohash: hash.to_string(),
                    });
                }
            }
        }
        Ok(failed)
    }

    fn query_all(&self) -> anyhow::Result<Vec<DownloadSnapshot>> {
        let result = self.rpc(
            "torrent_get",
            &serde_json::json!({
                "fields": [
                    "id", "hash_string", "name", "status",
                    "percent_done", "rate_download", "total_size",
                ],
            }),
        );
        let torrents = result
            .as_ref()
            .and_then(|r| r["torrents"].as_array())
            .cloned()
            .unwrap_or_default();

        let mut snapshots = Vec::new();
        for t in &torrents {
            let status = t["status"].as_i64().unwrap_or(-1);
            let state = match status {
                0 => {
                    let pct: f64 = t["percent_done"].as_f64().unwrap_or(0.0);
                    if pct >= 1.0 {
                        DownloadState::Completed
                    } else {
                        DownloadState::Paused
                    }
                }
                3 | 4 => DownloadState::Downloading,
                6 => DownloadState::Seeding,
                1 | 2 => DownloadState::Checking,
                _ => DownloadState::Waiting,
            };

            snapshots.push(DownloadSnapshot {
                infohash: t["hash_string"].as_str().unwrap_or("").to_string(),
                state,
                progress: t["percent_done"].as_f64().unwrap_or(0.0) as f32,
                speed: t["rate_download"].as_u64().unwrap_or(0),
                size: t["total_size"].as_u64().unwrap_or(0),
                name: t["name"].as_str().unwrap_or("").to_string(),
            });
        }
        Ok(snapshots)
    }

    fn check_connection(&self) -> anyhow::Result<()> {
        let _ = self.rpc(
            "session_get",
            &serde_json::json!({ "fields": ["version"] }),
        )
        .ok_or_else(|| anyhow::anyhow!("transmission: session_get failed"))?;
        Ok(())
    }
}
