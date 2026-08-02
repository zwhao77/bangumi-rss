//! qBittorrent downloader — Web API client behind the `TorrentDownloader` trait.
//!
//! Uses qBittorrent's REST API (v2).  No gid mapping needed — qBittorrent
//! uses infohash (called "hash") as the primary identifier.
//!
//! Requires qBittorrent ≥ 5.0 (HTTP Basic Auth).  v4.x cookie-based auth
//! is not supported.

use crate::traits::{OpResult, TorrentDownloader};
use crate::types::{CompletedDownload, DownloadSnapshot, DownloadState, TorrentFile};

use base64::Engine as _;

/// Concrete downloader backed by qBittorrent's Web API (≥ v5.0).
///
/// Stateless: every request carries an `Authorization: Basic` header derived
/// from the configured username and password — no cookie, no session, no Mutex.
pub struct QbittorrentDownloader {
    api_url: String,
    username: String,
    password: String,
}

impl QbittorrentDownloader {
    pub fn from_config(url: String, username: String, password: String) -> Self {
        Self {
            api_url: url.trim_end_matches('/').to_string(),
            username,
            password,
        }
    }

    /// Build the `Authorization: Basic` header value once per request.
    fn auth_value(&self) -> String {
        format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD
                .encode(format!("{}:{}", self.username, self.password))
        )
    }

    // ── HTTP helpers ──

    fn timeout() -> std::time::Duration {
        std::time::Duration::from_secs(crate::config::HTTP_TIMEOUT_SECS)
    }

    fn get(&self, path: &str) -> Option<serde_json::Value> {
        let url = format!("{}/api/v2/{}", self.api_url, path);
        ureq::get(&url)
            .set("Authorization", &self.auth_value())
            .timeout(Self::timeout())
            .call()
            .ok()?
            .into_json()
            .ok()
    }

    fn post_form(&self, path: &str, fields: &[(&str, &str)]) -> anyhow::Result<serde_json::Value> {
        let url = format!("{}/api/v2/{}", self.api_url, path);
        let body: String = fields
            .iter()
            .map(|(k, v)| format!("{}={}", urlencoding(k), urlencoding(v)))
            .collect::<Vec<_>>()
            .join("&");
        let resp = ureq::post(&url)
            .set("Authorization", &self.auth_value())
            .set("Content-Type", "application/x-www-form-urlencoded")
            .timeout(Self::timeout())
            .send_string(&body)
            .map_err(|e| anyhow::anyhow!("qbittorrent: POST {path} failed: {e}"))?;
        let status = resp.status();
        let text = resp.into_string().unwrap_or_default();
        if !(200..300).contains(&status) {
            anyhow::bail!("qbittorrent: POST {path} returned {status}: {text}");
        }
        // Write endpoints return an empty body / "Ok."; only add returns JSON → treat empty as Null
        if text.trim().is_empty() || text.trim() == "Ok." {
            Ok(serde_json::Value::Null)
        } else {
            serde_json::from_str(&text)
                .map_err(|e| anyhow::anyhow!("qbittorrent: POST {path} returned non-JSON: {e}"))
        }
    }

    fn post_multipart(
        &self,
        path: &str,
        file_name: &str,
        data: &[u8],
        extra: &[(&str, &str)],
    ) -> anyhow::Result<serde_json::Value> {
        let boundary = format!(
            "----WebKitFormBoundary{:016x}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        let mut body = Vec::new();

        // File part
        let header = format!(
            "--{}\r\nContent-Disposition: form-data; name=\"torrents\"; filename=\"{}\"\r\nContent-Type: application/x-bittorrent\r\n\r\n",
            boundary, file_name
        );
        body.extend_from_slice(header.as_bytes());
        body.extend_from_slice(data);
        body.extend_from_slice(b"\r\n");

        // Extra fields (savepath, etc.)
        for (k, v) in extra {
            let field = format!(
                "--{}\r\nContent-Disposition: form-data; name=\"{}\"\r\n\r\n{}\r\n",
                boundary, k, v
            );
            body.extend_from_slice(field.as_bytes());
        }

        // Closing boundary
        body.extend_from_slice(format!("--{}--\r\n", boundary).as_bytes());

        let url = format!("{}/api/v2/{}", self.api_url, path);
        let ct = format!("multipart/form-data; boundary={}", boundary);

        let resp = ureq::post(&url)
            .set("Authorization", &self.auth_value())
            .set("Content-Type", &ct)
            .timeout(Self::timeout())
            .send_bytes(&body)?;
        let status = resp.status();
        let text = resp.into_string().unwrap_or_default();
        if !(200..300).contains(&status) {
            anyhow::bail!("qbittorrent: multipart POST {path} returned {status}: {text}");
        }
        if text.trim().is_empty() || text.trim() == "Ok." {
            Ok(serde_json::Value::Null)
        } else {
            serde_json::from_str(&text).map_err(|e| {
                anyhow::anyhow!("qbittorrent: multipart POST {path} returned non-JSON: {e}")
            })
        }
    }

    /// Snapshot of all known torrent hashes (used to detect a newly added one).
    fn all_hashes(&self) -> std::collections::HashSet<String> {
        self.get("torrents/info?filter=all")
            .and_then(|r| r.as_array().cloned())
            .unwrap_or_default()
            .iter()
            .filter_map(|t| t["hash"].as_str().map(String::from))
            .collect()
    }

    /// Adding a .torrent by URL is async: `torrents/add` reports it as pending.
    /// Snapshot existing hashes, then poll until a new torrent appears (with timeout).
    fn wait_for_added(&self, dir: &str) -> anyhow::Result<String> {
        let before = self.all_hashes();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if std::time::Instant::now() > deadline {
                anyhow::bail!("qbittorrent: timed out waiting for torrent to appear in {dir}");
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
            let now = self.all_hashes();
            if let Some(h) = now.difference(&before).next() {
                return Ok(h.clone());
            }
        }
    }
}

fn urlencoding(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                result.push(b as char);
            }
            _ => {
                result.push_str(&format!("%{:02X}", b));
            }
        }
    }
    result
}

impl TorrentDownloader for QbittorrentDownloader {
    fn add_uri(&self, uri: &str, dir: &str) -> anyhow::Result<String> {
        let json = self.post_form(
            "torrents/add",
            &[("urls", uri), ("savepath", dir), ("autoTMM", "false")],
        )?;

        // Sync add (magnet / direct link) → the response carries the hash directly
        if let Some(h) = json["added_torrent_ids"]
            .as_array()
            .and_then(|a| a.first())
            .and_then(|h| h.as_str())
        {
            log::info!("[qbittorrent] add_uri: infohash={h}");
            return Ok(h.to_string());
        }

        // Async URL add → short polling fallback
        log::debug!("[qbittorrent] add_uri: pending, polling for torrent in {dir}");
        self.wait_for_added(dir)
    }

    fn add_torrent_bytes(&self, data: &[u8], dir: &str) -> anyhow::Result<String> {
        let json = self.post_multipart(
            "torrents/add",
            "download.torrent",
            data,
            &[("savepath", dir), ("autoTMM", "false")],
        )?;

        if let Some(h) = json["added_torrent_ids"]
            .as_array()
            .and_then(|a| a.first())
            .and_then(|h| h.as_str())
        {
            log::info!("[qbittorrent] add_torrent_bytes: infohash={h}");
            return Ok(h.to_string());
        }

        log::debug!("[qbittorrent] add_torrent_bytes: pending, polling for torrent in {dir}");
        self.wait_for_added(dir)
    }

    fn list_files(&self, infohash: &str) -> anyhow::Result<Vec<TorrentFile>> {
        // Query failure (network / HTTP error) → Err; query OK but no files → Ok(empty)
        let arr = self
            .get(&format!("torrents/files?hash={}", infohash))
            .and_then(|r| r.as_array().cloned())
            .ok_or_else(|| anyhow::anyhow!("qbittorrent: list_files failed for {infohash}"))?;
        Ok(arr
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
    ) -> anyhow::Result<OpResult> {
        self.post_form(
            "torrents/renameFile",
            &[
                ("hash", infohash),
                ("oldPath", old_path),
                ("newPath", new_name),
            ],
        )
        .map_err(|e| anyhow::anyhow!("qbittorrent: rename failed for {infohash}: {e}"))?;
        Ok(OpResult::Done)
    }

    fn move_files(&self, infohash: &str, new_location: &str) -> anyhow::Result<OpResult> {
        self.post_form(
            "torrents/setLocation",
            &[("hashes", infohash), ("location", new_location)],
        )
        .map_err(|e| anyhow::anyhow!("qbittorrent: move failed for {infohash}: {e}"))?;
        Ok(OpResult::Done)
    }

    fn pause(&self, infohash: &str) -> anyhow::Result<()> {
        self.post_form("torrents/stop", &[("hashes", infohash)])
            .map_err(|e| anyhow::anyhow!("qbittorrent: pause failed for {infohash}: {e}"))?;
        Ok(())
    }

    fn resume(&self, infohash: &str) -> anyhow::Result<()> {
        self.post_form("torrents/start", &[("hashes", infohash)])
            .map_err(|e| anyhow::anyhow!("qbittorrent: resume failed for {infohash}: {e}"))?;
        Ok(())
    }

    fn remove(&self, infohash: &str, delete_files: bool) -> anyhow::Result<()> {
        let df = if delete_files { "true" } else { "false" };
        self.post_form(
            "torrents/delete",
            &[("hashes", infohash), ("deleteFiles", df)],
        )
        .map_err(|e| anyhow::anyhow!("qbittorrent: remove failed for {infohash}: {e}"))?;
        Ok(())
    }

    fn poll_completed(&self) -> anyhow::Result<Vec<CompletedDownload>> {
        // A failed query must return Err — “no completed tasks” vs “query failed” is the caller’s call.
        let arr = self
            .get("torrents/info?filter=completed")
            .and_then(|r| r.as_array().cloned())
            .ok_or_else(|| anyhow::anyhow!("qbittorrent: poll_completed failed"))?;
        Ok(arr
            .iter()
            .filter_map(|t| {
                t["hash"].as_str().map(|h| CompletedDownload {
                    infohash: h.to_string(),
                })
            })
            .collect())
    }

    fn poll_failed(&self) -> anyhow::Result<Vec<CompletedDownload>> {
        // A failed query must return Err — “no failed tasks” vs “query failed” is the caller’s call.
        let arr = self
            .get("torrents/info?filter=errored")
            .and_then(|r| r.as_array().cloned())
            .ok_or_else(|| anyhow::anyhow!("qbittorrent: poll_failed failed"))?;
        Ok(arr
            .iter()
            .filter_map(|t| {
                t["hash"].as_str().map(|h| CompletedDownload {
                    infohash: h.to_string(),
                })
            })
            .collect())
    }

    fn query_all(&self) -> anyhow::Result<Vec<DownloadSnapshot>> {
        // When the downloader is unreachable, get() returns None — must error
        // instead of returning empty, or reconciliation would treat every Downloading
        // task as vanished and re-download everything.
        let arr = self
            .get("torrents/info")
            .and_then(|r| r.as_array().cloned())
            .ok_or_else(|| anyhow::anyhow!("qbittorrent: query_all failed"))?;

        Ok(arr
            .iter()
            .map(|t| {
                let hash = t["hash"].as_str().unwrap_or("").to_string();
                let name = t["name"].as_str().unwrap_or("").to_string();
                let total: u64 = t["size"].as_u64().unwrap_or(0);
                let _done: u64 = t["completed"].as_u64().unwrap_or(0);
                let progress = t["progress"].as_f64().unwrap_or(0.0) as f32;
                let speed: u64 = t["dlspeed"].as_u64().unwrap_or(0);

                let state = match t["state"].as_str().unwrap_or("") {
                    "uploading" | "stalledUP" | "queuedUP" | "checkingUP" => DownloadState::Seeding,
                    "downloading" | "stalledDL" | "queuedDL" | "checkingDL" | "metaDL"
                    | "forcedDL" | "forcedUP" | "allocating" | "moving" => {
                        if progress >= 1.0 {
                            DownloadState::Seeding
                        } else {
                            DownloadState::Downloading
                        }
                    }
                    "pausedUP" | "pausedDL" => DownloadState::Paused,
                    "error" | "missingFiles" | "unknown" => DownloadState::Failed,
                    _ => DownloadState::Waiting,
                };

                DownloadSnapshot {
                    infohash: hash,
                    state,
                    progress,
                    speed,
                    size: total,
                    name,
                }
            })
            .collect())
    }

    fn check_connection(&self) -> anyhow::Result<()> {
        let url = format!("{}/api/v2/app/version", self.api_url);
        match ureq::get(&url)
            .set("Authorization", &self.auth_value())
            .timeout(Self::timeout())
            .call()
        {
            Ok(resp) if resp.status() == 200 => Ok(()),
            Ok(resp) => anyhow::bail!("qbittorrent returned HTTP {}", resp.status()),
            Err(e) => anyhow::bail!("qbittorrent not reachable: {e}"),
        }
    }
}
