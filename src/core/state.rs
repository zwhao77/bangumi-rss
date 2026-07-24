use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use uuid::Uuid;

use crate::types::{AnimeIdentity, BangumiInfo, EpisodeRecord, RecordStatus};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Feed {
    pub id: Uuid,
    pub url: String,
    pub anime: AnimeIdentity,
    pub confirmed: bool,
    /// Bangumi metadata — fetched once on confirm, persisted in state.json.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bangumi_info: Option<BangumiInfo>,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppState {
    pub feeds: HashMap<Uuid, Feed>,

    /// infohash → episode record (tracks torrent from start → library).
    pub tracker: HashMap<String, EpisodeRecord>,

    /// Torrent URLs already submitted — prevents cross-feed duplicate downloads.
    /// Persisted so it survives restarts.
    pub seen_urls: HashSet<String>,

    /// Cached download list for API responses — updated by RefreshDownloads.
    #[serde(skip)]
    pub cached_downloads: Vec<crate::types::DownloadInfo>,

    pub download_dir: String,
    pub library_dir: String,
    pub webhook_url: Option<String>,
}

impl AppState {
    // ── persistence ──

    pub fn load(data_dir: &str) -> Option<Self> {
        let path = data_path(data_dir);
        if !path.exists() {
            return None;
        }
        let content = std::fs::read_to_string(&path).ok()?;
        serde_json::from_str(&content).ok()
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let path = data_path(&std::env::var("DATA_DIR").unwrap_or_default());
        let tmp = path.with_extension("tmp");

        let content = serde_json::to_string_pretty(self)?;
        std::fs::write(&tmp, content)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    // ── consuming builders ──

    /// Insert a new confirmed feed subscription.
    pub fn with_feed_added(mut self, feed: Feed) -> Self {
        self.feeds.insert(feed.id, feed);
        self
    }

    /// Confirm a feed's anime name, season, and optional Bangumi metadata.
    pub fn with_feed_confirmed(
        mut self,
        id: Uuid,
        name: String,
        season: u8,
        bangumi_info: Option<BangumiInfo>,
    ) -> Self {
        if let Some(f) = self.feeds.get_mut(&id) {
            f.anime.name = name;
            f.anime.season = season;
            f.confirmed = true;
            f.bangumi_info = bangumi_info;
        }
        self
    }

    /// Record that a download has started — insert or update tracker entry.
    pub fn with_download_started(mut self, record: EpisodeRecord) -> Self {
        self.tracker.insert(record.infohash.clone(), record);
        self
    }

    /// Update tracker: episode resolved by tokenizer.
    pub fn with_episode_resolved(
        mut self,
        infohash: &str,
        episode: u32,
        library_path: String,
    ) -> Self {
        if let Some(r) = self.tracker.get_mut(infohash) {
            r.key.episode = episode;
            r.library_path = Some(library_path);
            r.status = RecordStatus::Resolved;
        }
        self
    }

    /// Mark a download as moved into library.
    pub fn with_download_in_library(mut self, infohash: &str) -> Self {
        if let Some(r) = self.tracker.get_mut(infohash) {
            r.status = RecordStatus::InLibrary;
        }
        self
    }

    /// Remove a feed and all related tracker entries.
    pub fn with_feed_removed(mut self, id: Uuid) -> Self {
        self.feeds.remove(&id);
        self.tracker.retain(|_, r| r.feed_id != id);
        self
    }

    /// Mark a torrent URL as already submitted — idempotent.
    pub fn with_url_seen(mut self, url: &str) -> Self {
        self.seen_urls.insert(url.to_string());
        self
    }

    /// Check if a torrent URL was already submitted.
    pub fn has_url(&self, url: &str) -> bool {
        self.seen_urls.contains(url)
    }

    /// Replace the cached download list.
    pub fn with_downloads_cached(mut self, downloads: Vec<crate::types::DownloadInfo>) -> Self {
        self.cached_downloads = downloads;
        self
    }
}

fn data_path(data_dir: &str) -> PathBuf {
    if data_dir.is_empty() {
        PathBuf::from("state.json")
    } else {
        PathBuf::from(data_dir).join("state.json")
    }
}
