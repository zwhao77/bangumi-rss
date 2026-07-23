//! Aria2 downloader — JSON-RPC client behind the `TorrentDownloader` trait.
//!
//! Stateless: every infohash → gid lookup rebuilds the mapping from
//! `tellActive` + `tellStopped`.  Two extra RPC calls per `HandleCompleted`
//! (~10-15 ms) are negligible at the download-completion rate.

use std::collections::HashMap;

use crate::traits::TorrentDownloader;
use crate::types::{CompletedDownload, DownloadSnapshot, TorrentFile};

/// Concrete downloader backed by aria2's JSON-RPC API.
///
/// Holds no mutable state — safe to share via `Arc`.
pub struct Aria2Downloader {
    rpc_url: String,
}

impl Aria2Downloader {
    pub fn with_rpc_url(rpc_url: String) -> Self {
        Self { rpc_url }
    }

    // ── Low-level JSON-RPC ──

    fn rpc(&self, method: &str, params: &[serde_json::Value]) -> Option<serde_json::Value> {
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "id": "1",
            "method": format!("aria2.{method}"),
            "params": params,
        });
        let resp = ureq::post(&self.rpc_url).send_json(payload).ok()?;
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
                Some(TorrentFile { name })
            })
            .collect())
    }

    fn rename_file(&self, infohash: &str, _old_path: &str, new_name: &str) -> anyhow::Result<bool> {
        let gid = self.with_gid(infohash)?;
        let result = self.rpc(
            "changeOption",
            &[
                serde_json::json!(gid),
                serde_json::json!({ "out": new_name }),
            ],
        );
        Ok(result.is_some())
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Child, Command};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU16, Ordering};

    // ── Self-contained aria2c test harness ──

    /// Port counter — each harness gets a unique port.
    static NEXT_PORT: AtomicU16 = AtomicU16::new(17000);

    /// Spawns an aria2c daemon on a unique port, provides a `downloader()`,
    /// and cleans up when dropped.  No manual setup needed.
    struct Aria2Harness {
        dir: tempfile::TempDir,
        port: u16,
        child: Mutex<Option<Child>>,
    }

    impl Aria2Harness {
        fn start() -> Self {
            let port = NEXT_PORT.fetch_add(1, Ordering::Relaxed);
            let dir = tempfile::TempDir::new().expect("temp dir");

            // Write aria2 config
            let conf = format!(
                "enable-rpc=true\n\
                 rpc-listen-port={port}\n\
                 dir={}/downloads\n\
                 log={}/aria2.log\n\
                 log-level=warn\n\
                 dht-enabled=false\n\
                 bt-tracker=\n",
                dir.path().display(),
                dir.path().display()
            );
            let conf_path = dir.path().join("aria2.conf");
            std::fs::write(&conf_path, conf).expect("write config");
            std::fs::create_dir_all(dir.path().join("downloads")).expect("mkdir downloads");

            // Spawn aria2c
            let child = Command::new("aria2c")
                .arg("--conf-path")
                .arg(&conf_path)
                .arg("--daemon=true")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .expect("spawn aria2c");

            let harness = Self {
                dir,
                port,
                child: Mutex::new(Some(child)),
            };

            // Wait for RPC to become available (poll getVersion).
            let dl = harness.downloader();
            for _ in 0..30 {
                if dl.rpc("getVersion", &[]).is_some() {
                    println!("[harness] aria2c ready on port {port}");
                    return harness;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            panic!("aria2c did not become ready within 3s");
        }

        fn downloader(&self) -> Aria2Downloader {
            Aria2Downloader {
                rpc_url: format!("http://localhost:{}/jsonrpc", self.port),
            }
        }
    }

    impl Drop for Aria2Harness {
        fn drop(&mut self) {
            if let Some(mut child) = self.child.lock().unwrap().take() {
                let _ = child.kill();
                let _ = child.wait();
                println!("[harness] aria2c stopped");
            }
        }
    }

    // ── Test helper ──

    /// A tiny valid torrent file for testing.
    fn tiny_torrent_bytes() -> Vec<u8> {
        include_bytes!("../../res/test.torrent").to_vec()
    }

    // ── Unit tests ──

    #[test]
    fn with_gid_miss_returns_error() {
        let dl = Aria2Downloader {
            rpc_url: String::new(),
        };
        let result = dl.with_gid("deadbeef");
        assert!(result.is_err());
    }

    // ── Full-flow live test (auto-spawns aria2c) ──

    #[test]
    fn test_live_full_flow() {
        let h = Aria2Harness::start();
        let dl = h.downloader();
        let test_dir = h.dir.path().join("downloads");
        let test_dir = test_dir.to_str().unwrap();

        // ── 1. Fresh aria2: query_all succeeds ──
        assert!(
            dl.query_all().is_ok(),
            "query_all should succeed on fresh aria2"
        );

        // ── 2. Fresh aria2: no completed / failed tasks ──
        assert!(dl.poll_completed().unwrap().is_empty());
        assert!(dl.poll_failed().unwrap().is_empty());

        // ── 3. Add torrent → verify it appears in query_all ──
        let data = tiny_torrent_bytes();
        let infohash = dl.add_torrent_bytes(&data, test_dir).unwrap();
        assert!(
            !infohash.is_empty(),
            "add_torrent_bytes should return infohash"
        );
        println!("[test] added: {}", &infohash[..infohash.len().min(16)]);

        std::thread::sleep(std::time::Duration::from_millis(500));

        let snapshots = dl.query_all().unwrap();
        assert!(
            snapshots.iter().any(|s| s.infohash == infohash),
            "added torrent should appear in query_all"
        );

        // ── 4. with_gid resolves successfully ──
        let gid = dl.with_gid(&infohash).unwrap();
        assert!(!gid.is_empty());
        println!(
            "[test] with_gid: {} → {}",
            &infohash[..infohash.len().min(16)],
            gid
        );

        // ── 5. list_files returns files ──
        let files = dl.list_files(&infohash).unwrap();
        assert!(!files.is_empty(), "should list files");
        println!("[test] list_files: {:?}", files);

        // ── 6. Unknown infohash → error ──
        assert!(
            dl.with_gid("0000000000000000000000000000000000000000")
                .is_err()
        );

        // ── 7. fetch_gid_map_range ──
        let map = dl.fetch_gid_map_range(0, 100);
        println!("[test] fetch_gid_map_range(0,100): {} entries", map.len());

        // ── 8. Benchmark: add 19 more, measure with_gid ──
        let mut hashes = vec![infohash];
        for i in 0..19 {
            match dl.add_torrent_bytes(&data, test_dir) {
                Ok(ih) => hashes.push(ih),
                Err(e) => eprintln!("[bench] add #{} failed: {e}", i + 1),
            }
        }
        std::thread::sleep(std::time::Duration::from_secs(1));

        // Warm-up
        let _ = dl.with_gid(&hashes[0]);

        let mut times = Vec::new();
        for i in 0..20 {
            let start = std::time::Instant::now();
            assert!(dl.with_gid(&hashes[i % hashes.len()]).is_ok());
            times.push(start.elapsed());
        }
        times.sort();
        println!(
            "[bench] {} tasks: min={:>8.2?} median={:>8.2?} avg={:>8.2?}",
            hashes.len(),
            times[0],
            times[times.len() / 2],
            times.iter().sum::<std::time::Duration>() / times.len() as u32,
        );

        let start = std::time::Instant::now();
        let _ = dl.with_gid("0000000000000000000000000000000000000000");
        println!("[bench] miss (full scan): {:>8.2?}", start.elapsed());
    }
}
