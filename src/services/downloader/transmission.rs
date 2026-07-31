//! Transmission downloader — JSON-RPC 2.0 client behind `TorrentDownloader`.
//!
//! Transmission 4.1.0+ uses JSON-RPC 2.0 (snake_case methods).
//! CSRF protection: first request returns HTTP 409 with `X-Transmission-Session-Id`;
//! subsequent requests include that header.

use std::sync::Mutex;

use crate::traits::{OpResult, TorrentDownloader};
use crate::types::{CompletedDownload, DownloadSnapshot, DownloadState, TorrentFile};
use base64::Engine;

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

    /// Build a ureq POST request with common headers (Content-Type, auth, cached CSRF session).
    fn build_request(&self) -> ureq::Request {
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

        // CSRF session id: always read from cache.
        // The 409 handler writes the new id to cache before retrying,
        // so the retry path picks it up automatically.
        let sid = {
            self.session_id
                .lock()
                .unwrap_or_else(|poisoned| {
                    log::warn!("[transmission] session_id mutex was poisoned, recovering");
                    poisoned.into_inner()
                })
                .clone()
        };
        if let Some(ref s) = sid {
            log::debug!("[transmission] using session_id: {s}");
            req.set("X-Transmission-Session-Id", s)
        } else {
            req
        }
    }

    /// Send a JSON-RPC request; returns the parsed "result" field on success.
    /// Handles CSRF 409 transparently via automatic retry.
    fn rpc(&self, method: &str, params: &serde_json::Value) -> Option<serde_json::Value> {
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        });
        let payload_str = serde_json::to_string(&payload).ok()?;
        log::debug!("[transmission] >>> rpc call: {method}");

        // First attempt: use cached session_id (if any).
        match self.build_request().send_string(&payload_str) {
            Ok(resp) => {
                let body: serde_json::Value = resp.into_json().ok()?;
                if body.get("error").is_some() {
                    log::warn!(
                        "[transmission] rpc {method}: error in response: {}",
                        body["error"]
                    );
                    return None;
                }
                log::debug!("[transmission] <<< rpc {method} result: {}", body["result"]);
                Some(body["result"].clone())
            }
            Err(ureq::Error::Status(409, resp)) => {
                // CSRF challenge: extract session id from headers and retry once.
                let new_sid = resp
                    .header("X-Transmission-Session-Id")
                    .map(String::from)
                    .unwrap_or_default();
                if new_sid.is_empty() {
                    log::warn!(
                        "[transmission] rpc {method}: got 409 but no X-Transmission-Session-Id header"
                    );
                    return None;
                }
                log::info!("[transmission] CSRF 409, got session_id, retrying once");
                {
                    match self.session_id.lock() {
                        Ok(mut guard) => *guard = Some(new_sid.clone()),
                        Err(poisoned) => {
                            log::warn!(
                                "[transmission] session_id mutex poisoned, recovering and updating"
                            );
                            *poisoned.into_inner() = Some(new_sid.clone());
                        }
                    }
                }

                match self.build_request().send_string(&payload_str) {
                    Ok(retry_resp) => {
                        let retry_body: serde_json::Value = retry_resp.into_json().ok()?;
                        log::debug!("[transmission] <<< rpc {method} retry response: {retry_body}");
                        if retry_body.get("error").is_some() {
                            log::warn!(
                                "[transmission] rpc {method} retry: error in response: {}",
                                retry_body["error"]
                            );
                            return None;
                        }
                        Some(retry_body["result"].clone())
                    }
                    Err(e) => {
                        log::warn!("[transmission] rpc {method} retry after 409 also failed: {e}");
                        None
                    }
                }
            }
            Err(e) => {
                log::warn!("[transmission] rpc {method} request failed: {e}");
                None
            }
        }
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

        let hash = added["hash_string"]
            .as_str()
            .map(String::from)
            .ok_or_else(|| {
                anyhow::anyhow!("transmission: no hash_string in torrent_add response")
            })?;
        log::info!("[transmission] add_uri: uri={uri}, infohash={hash}");
        Ok(hash)
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
        let is_dup = result
            .as_ref()
            .and_then(|r| r["torrent_duplicate"].as_object())
            .is_some();
        let added = result
            .as_ref()
            .and_then(|r| r["torrent_added"].as_object())
            .or_else(|| {
                result
                    .as_ref()
                    .and_then(|r| r["torrent_duplicate"].as_object())
            })
            .ok_or_else(|| {
                anyhow::anyhow!("transmission: torrent_add (bytes) returned no result")
            })?;

        let hash = added["hash_string"]
            .as_str()
            .map(String::from)
            .ok_or_else(|| anyhow::anyhow!("transmission: no hash_string in add response"))?;
        if is_dup {
            log::info!(
                "[transmission] add_torrent_bytes: duplicate torrent detected, infohash={hash}"
            );
        } else {
            log::info!("[transmission] add_torrent_bytes: added, infohash={hash}");
        }
        Ok(hash)
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

        let files = torrent["files"].as_array().cloned().unwrap_or_default();

        let file_list: Vec<TorrentFile> = files
            .iter()
            .map(|f| {
                let path = f["name"].as_str().unwrap_or("").to_string();
                let name = std::path::Path::new(&path)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                TorrentFile { path, name }
            })
            .collect();
        log::debug!(
            "[transmission] list_files: infohash={infohash}, {} file(s)",
            file_list.len()
        );
        for (i, f) in file_list.iter().enumerate() {
            log::trace!(
                "[transmission] list_files:   [{i}] path={}, name={}",
                f.path,
                f.name
            );
        }
        Ok(file_list)
    }

    fn rename_file(
        &self,
        infohash: &str,
        old_path: &str,
        new_name: &str,
    ) -> anyhow::Result<OpResult> {
        let result = self.rpc(
            "torrent_rename_path",
            &serde_json::json!({
                "ids": [infohash],
                "path": old_path,
                "name": new_name,
            }),
        );
        match result {
            Some(_) => {
                log::info!(
                    "[transmission] rename_file: infohash={infohash}, old_path={old_path}, new_name={new_name}"
                );
                Ok(OpResult::Done)
            }
            None => {
                log::warn!("[transmission] rename_file failed: infohash={infohash}");
                Err(anyhow::anyhow!(
                    "transmission: rename_file failed for {infohash}"
                ))
            }
        }
    }

    fn move_files(&self, infohash: &str, new_location: &str) -> anyhow::Result<OpResult> {
        let result = self.rpc(
            "torrent_set_location",
            &serde_json::json!({
                "ids": [infohash],
                "location": new_location,
                "move": true,
            }),
        );
        match result {
            Some(_) => {
                log::info!(
                    "[transmission] move_files: infohash={infohash}, location={new_location}"
                );
                Ok(OpResult::Done)
            }
            None => {
                log::warn!("[transmission] move_files failed: infohash={infohash}");
                Err(anyhow::anyhow!(
                    "transmission: move_files failed for {infohash}"
                ))
            }
        }
    }

    fn pause(&self, infohash: &str) -> anyhow::Result<()> {
        let result = self.rpc(
            "torrent_stop",
            &serde_json::json!({
                "ids": [infohash],
            }),
        );
        match result {
            Some(_) => {
                log::info!("[transmission] pause: infohash={infohash}");
                Ok(())
            }
            None => {
                log::warn!("[transmission] pause failed: infohash={infohash}");
                Err(anyhow::anyhow!("transmission: pause failed for {infohash}"))
            }
        }
    }

    fn resume(&self, infohash: &str) -> anyhow::Result<()> {
        let result = self.rpc(
            "torrent-start-now",
            &serde_json::json!({
                "ids": [infohash],
            }),
        );
        match result {
            Some(_) => {
                log::info!("[transmission] resume: infohash={infohash}");
                Ok(())
            }
            None => {
                log::warn!("[transmission] resume failed: infohash={infohash}");
                Err(anyhow::anyhow!(
                    "transmission: resume failed for {infohash}"
                ))
            }
        }
    }

    fn remove(&self, infohash: &str, delete_files: bool) -> anyhow::Result<()> {
        let result = self.rpc(
            "torrent_remove",
            &serde_json::json!({
                "ids": [infohash],
                "delete_local_data": delete_files,
            }),
        );
        match result {
            Some(_) => {
                log::info!(
                    "[transmission] remove: infohash={infohash}, delete_files={delete_files}"
                );
                Ok(())
            }
            None => {
                log::warn!("[transmission] remove failed: infohash={infohash}");
                Err(anyhow::anyhow!(
                    "transmission: remove failed for {infohash}"
                ))
            }
        }
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
            if (status == 0 || status == 6)
                && percent >= 1.0
                && let Some(hash) = t["hash_string"].as_str()
            {
                completed.push(CompletedDownload {
                    infohash: hash.to_string(),
                });
            }
        }
        log::debug!(
            "[transmission] poll_completed: {} torrent(s) completed",
            completed.len()
        );
        for c in &completed {
            log::info!("[transmission] poll_completed: infohash={}", c.infohash);
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
            let err_str = t["error_string"].as_str().unwrap_or("");
            if status == 0
                && error >= 3
                && let Some(hash) = t["hash_string"].as_str()
            {
                log::warn!(
                    "[transmission] poll_failed: infohash={hash}, error={error}, error_string={err_str}"
                );
                failed.push(CompletedDownload {
                    infohash: hash.to_string(),
                });
            }
        }
        log::debug!(
            "[transmission] poll_failed: {} torrent(s) failed",
            failed.len()
        );
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

            let infohash = t["hash_string"].as_str().unwrap_or("").to_string();
            let name = t["name"].as_str().unwrap_or("").to_string();
            let progress = t["percent_done"].as_f64().unwrap_or(0.0) as f32;
            snapshots.push(DownloadSnapshot {
                infohash: infohash.clone(),
                state: state.clone(),
                progress,
                speed: t["rate_download"].as_u64().unwrap_or(0),
                size: t["total_size"].as_u64().unwrap_or(0),
                name: name.clone(),
            });
            log::trace!(
                "[transmission] query_all: infohash={infohash}, name={name}, state={state:?}, progress={progress:.2}, status_code={status}"
            );
        }
        log::debug!("[transmission] query_all: {} torrent(s)", snapshots.len());
        Ok(snapshots)
    }

    fn check_connection(&self) -> anyhow::Result<()> {
        let result = self.rpc("session_get", &serde_json::json!({ "fields": ["version"] }));
        match result {
            Some(v) => {
                let version = v["version"].as_str().unwrap_or("unknown");
                log::info!("[transmission] check_connection: connected, version={version}");
                Ok(())
            }
            None => {
                log::warn!("[transmission] check_connection: session_get failed");
                Err(anyhow::anyhow!("transmission: session_get failed"))
            }
        }
    }
}
