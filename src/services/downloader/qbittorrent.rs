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

    fn post_form(&self, path: &str, fields: &[(&str, &str)]) -> Option<serde_json::Value> {
        let url = format!("{}/api/v2/{}", self.api_url, path);
        let body: String = fields
            .iter()
            .map(|(k, v)| format!("{}={}", urlencoding(k), urlencoding(v)))
            .collect::<Vec<_>>()
            .join("&");
        ureq::post(&url)
            .set("Authorization", &self.auth_value())
            .set("Content-Type", "application/x-www-form-urlencoded")
            .timeout(Self::timeout())
            .send_string(&body)
            .ok()?
            .into_json()
            .ok()
    }

    fn post_multipart(
        &self,
        path: &str,
        file_name: &str,
        data: &[u8],
        extra: &[(&str, &str)],
    ) -> anyhow::Result<()> {
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
        if resp.status() != 200 {
            anyhow::bail!(
                "qbittorrent: multipart POST {} returned {}",
                path,
                resp.status()
            );
        }

        Ok(())
    }

    /// After adding a torrent, find its infohash by querying the most recent one.
    fn most_recent_hash(&self) -> anyhow::Result<String> {
        let arr = self
            .get("torrents/info?sort=added_on&reverse=true&limit=1")
            .and_then(|r| r.as_array().cloned())
            .unwrap_or_default();
        arr.first()
            .and_then(|t| t["hash"].as_str().map(String::from))
            .ok_or_else(|| anyhow::anyhow!("qbittorrent: could not find added torrent"))
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
        self.post_form(
            "torrents/add",
            &[("urls", uri), ("savepath", dir), ("autoTMM", "false")],
        );
        // qBittorrent returns "Ok." on success — we need to find the hash.
        self.most_recent_hash()
    }

    fn add_torrent_bytes(&self, data: &[u8], dir: &str) -> anyhow::Result<String> {
        self.post_multipart(
            "torrents/add",
            "download.torrent",
            data,
            &[("savepath", dir), ("autoTMM", "false")],
        )?;
        self.most_recent_hash()
    }

    fn list_files(&self, infohash: &str) -> anyhow::Result<Vec<TorrentFile>> {
        let arr = self
            .get(&format!("torrents/files?hash={}", infohash))
            .and_then(|r| r.as_array().cloned())
            .unwrap_or_default();
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
        .ok_or_else(|| anyhow::anyhow!("qbittorrent: rename failed for {infohash}"))?;
        Ok(OpResult::Done)
    }

    fn move_files(&self, infohash: &str, new_location: &str) -> anyhow::Result<OpResult> {
        self.post_form(
            "torrents/setLocation",
            &[("hashes", infohash), ("location", new_location)],
        )
        .ok_or_else(|| anyhow::anyhow!("qbittorrent: move failed for {infohash}"))?;
        Ok(OpResult::Done)
    }

    fn pause(&self, infohash: &str) -> anyhow::Result<()> {
        self.post_form("torrents/stop", &[("hashes", infohash)])
            .ok_or_else(|| anyhow::anyhow!("qbittorrent: pause failed for {infohash}"))?;
        Ok(())
    }

    fn resume(&self, infohash: &str) -> anyhow::Result<()> {
        self.post_form("torrents/start", &[("hashes", infohash)])
            .ok_or_else(|| anyhow::anyhow!("qbittorrent: resume failed for {infohash}"))?;
        Ok(())
    }

    fn remove(&self, infohash: &str, delete_files: bool) -> anyhow::Result<()> {
        let df = if delete_files { "true" } else { "false" };
        self.post_form(
            "torrents/delete",
            &[("hashes", infohash), ("deleteFiles", df)],
        )
        .ok_or_else(|| anyhow::anyhow!("qbittorrent: remove failed for {infohash}"))?;
        Ok(())
    }

    fn poll_completed(&self) -> anyhow::Result<Vec<CompletedDownload>> {
        let arr = self
            .get("torrents/info?filter=completed")
            .and_then(|r| r.as_array().cloned())
            .unwrap_or_default();
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
        let arr = self
            .get("torrents/info?filter=errored")
            .and_then(|r| r.as_array().cloned())
            .unwrap_or_default();
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
        let arr = self
            .get("torrents/info")
            .and_then(|r| r.as_array().cloned())
            .unwrap_or_default();

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
