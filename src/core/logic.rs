//! Pure reducer — no I/O, no side effects.
//!
//! Every function takes `&AppState` + an `Event` and returns
//! `(AppState, Vec<Effect>)`.  The caller owns and replaces the state.

use crate::core::effect::Effect;
use crate::core::event::{DownloadStatus, Event};
use crate::core::state::{AppState, Feed};
use crate::types::{
    AnimeIdentity, ApiResponse, BangumiInfo, DownloadInfo, DownloadSnapshot, EpisodeKey,
    EpisodeRecord, FeedInfo, RecordStatus, RssItem,
};
use uuid::Uuid;

/// Dispatch an event against the current (read-only) state,
/// producing a new state snapshot and effects to execute.
pub fn reduce(state: &AppState, event: Event) -> (AppState, Vec<Effect>) {
    match event {
        Event::RssTickAll => (state.clone(), reduce_rss_tick_all(state)),
        Event::RssItemsFetched {
            feed_id,
            items,
            download_dir,
        } => reduce_rss_items_fetched(state, feed_id, items, &download_dir),
        Event::PollDownloader => (state.clone(), reduce_poll_downloader()),
        Event::DownloadStarted {
            infohash,
            feed_id,
            torrent_url,
        } => reduce_download_started(state, infohash, feed_id, torrent_url),
        Event::DownloaderNotification { infohash, status } => {
            reduce_downloader_notification(state, infohash, status)
        }
        Event::EpisodeCompleted {
            infohash,
            episode,
            library_path,
        } => reduce_episode_completed(state, &infohash, episode, library_path),
        Event::UserConfirm {
            feed_id,
            name,
            season,
            bangumi_info,
            reply_tx,
        } => reduce_user_confirm(state, feed_id, name, season, bangumi_info, reply_tx),
        Event::ConfirmFeed {
            url,
            name,
            season,
            bangumi_info,
            reply_tx,
        } => reduce_confirm_feed(state, url, name, season, bangumi_info, reply_tx),
        Event::ApiListFeeds { reply_tx } => reduce_api_list_feeds(state, reply_tx),
        Event::ApiRemoveFeed { feed_id, reply_tx } => {
            reduce_api_remove_feed(state, feed_id, reply_tx)
        }
        Event::ApiListDownloads { reply_tx } => reduce_api_list_downloads(state, reply_tx),
        Event::RefreshDownloads => reduce_refresh_downloads(state),
        Event::DownloadsRefreshed { snapshots } => reduce_downloads_refreshed(state, snapshots),
    }
}

// ── Per-event reducers ──

/// Tick all subscribed feeds → emit one FetchRss effect per confirmed feed.
fn reduce_rss_tick_all(state: &AppState) -> Vec<Effect> {
    state
        .feeds
        .iter()
        .filter(|(_, f)| f.confirmed)
        .map(|(&id, feed)| Effect::FetchRss {
            url: feed.url.clone(),
            feed_id: id,
            download_dir: state.download_dir.clone(),
        })
        .collect()
}

/// Tick a single feed → FetchRss effect (used by tests).
#[cfg(test)]
fn reduce_rss_tick(state: &AppState, feed_id: Uuid) -> Vec<Effect> {
    match state.feeds.get(&feed_id) {
        Some(feed) if feed.confirmed => vec![Effect::FetchRss {
            url: feed.url.clone(),
            feed_id,
            download_dir: state.download_dir.clone(),
        }],
        _ => {
            log::warn!("RssTick for unknown/unconfirmed feed: {feed_id}");
            vec![]
        }
    }
}

/// Executor fetched RSS items → filter unseen URLs, produce AddTorrent effects.
fn reduce_rss_items_fetched(
    state: &AppState,
    feed_id: Uuid,
    items: Vec<RssItem>,
    download_dir: &str,
) -> (AppState, Vec<Effect>) {
    let new_state = state.clone();
    let mut effects = Vec::new();

    for item in &items {
        if item.torrent_url.is_empty() || new_state.has_url(&item.torrent_url) {
            continue;
        }
        // Reject batch torrents (e.g. "01-12").
        if crate::utils::tokenizer::is_batch_title(&item.title) {
            log::debug!("skip batch: {}", &item.title[..item.title.len().min(80)]);
            continue;
        }
        effects.push(Effect::AddTorrent {
            torrent_url: item.torrent_url.clone(),
            save_path: format!("{download_dir}/{feed_id}"),
            feed_id,
        });
    }

    if effects.is_empty() {
        log::debug!(
            "RssItemsFetched: all {} items already seen for feed={feed_id}",
            items.len()
        );
    }

    (new_state, effects)
}

/// Periodic download poll → emit poll effects.
fn reduce_poll_downloader() -> Vec<Effect> {
    vec![Effect::PollCompleted, Effect::PollFailed]
}

fn reduce_download_started(
    state: &AppState,
    infohash: String,
    feed_id: Uuid,
    torrent_url: String,
) -> (AppState, Vec<Effect>) {
    // Dedup: already tracked.
    if state.tracker.contains_key(&infohash) {
        return (state.clone(), vec![]);
    }

    let feed = match state.feeds.get(&feed_id) {
        Some(f) => f,
        None => return (state.clone(), vec![]),
    };

    let record = EpisodeRecord {
        infohash: infohash.clone(),
        torrent_url: torrent_url.clone(),
        feed_id,
        key: EpisodeKey {
            anime: feed.anime.clone(),
            episode: 0,
        },
        status: RecordStatus::Downloading,
        library_path: None,
    };

    let new_state = state
        .clone()
        .with_download_started(record)
        .with_url_seen(&torrent_url);
    (new_state, vec![])
}

/// A download completed or failed.
fn reduce_downloader_notification(
    state: &AppState,
    infohash: String,
    status: DownloadStatus,
) -> (AppState, Vec<Effect>) {
    match status {
        DownloadStatus::Completed => {
            log::info!(
                "download completed: {}",
                &infohash[..infohash.len().min(16)]
            );
            let record = match state.tracker.get(&infohash) {
                Some(r) => r,
                None => {
                    log::warn!("unknown download completed: {infohash}");
                    return (state.clone(), vec![]);
                }
            };

            let effects = vec![Effect::HandleCompleted {
                infohash,
                feed_id: record.feed_id,
                anime: record.key.anime.clone(),
                library_dir: state.library_dir.clone(),
                download_dir: state.download_dir.clone(),
                expected_episode: record.key.episode,
            }];

            (state.clone(), effects)
        }
        DownloadStatus::Failed => {
            log::warn!("download failed: {infohash}");
            let mut new_state = state.clone();
            new_state.tracker.remove(&infohash);
            (new_state, vec![])
        }
    }
}

/// Executor resolved episode + moved file to library.
fn reduce_episode_completed(
    state: &AppState,
    infohash: &str,
    episode: u32,
    library_path: String,
) -> (AppState, Vec<Effect>) {
    let record = match state.tracker.get(infohash) {
        Some(r) => r,
        None => {
            log::warn!("EpisodeCompleted for unknown infohash: {infohash}");
            return (state.clone(), vec![]);
        }
    };

    let effects = vec![Effect::Notify {
        title: format!("{} 下载完成", record.key.anime.name),
        body: format!("第 {episode} 集已移动 → {library_path}"),
    }];

    let new_state = state
        .clone()
        .with_episode_resolved(infohash, episode, library_path)
        .with_download_in_library(infohash);

    // Place effects after state update so save happens first.
    (new_state, effects)
}

/// User confirmed the anime name + season via the web UI.
fn reduce_user_confirm(
    state: &AppState,
    feed_id: Uuid,
    name: String,
    season: u8,
    bangumi_info: Option<BangumiInfo>,
    reply_tx: crossbeam_channel::Sender<ApiResponse>,
) -> (AppState, Vec<Effect>) {
    let exists = state.feeds.contains_key(&feed_id);
    let new_state = state
        .clone()
        .with_feed_confirmed(feed_id, name, season, bangumi_info);
    let _ = reply_tx.send(ApiResponse {
        success: exists,
        message: if exists {
            "updated".into()
        } else {
            format!("feed {feed_id} not found")
        },
    });
    (new_state, vec![])
}

/// API: request RSS preview — logic emits effect, executor replies directly.
/// API: confirm a feed subscription — create Feed with UUID.
fn reduce_confirm_feed(
    state: &AppState,
    url: String,
    name: String,
    season: u8,
    bangumi_info: Option<BangumiInfo>,
    reply_tx: crossbeam_channel::Sender<ApiResponse>,
) -> (AppState, Vec<Effect>) {
    if name.trim().is_empty() {
        let _ = reply_tx.send(ApiResponse {
            success: false,
            message: "name cannot be empty".into(),
        });
        return (state.clone(), vec![]);
    }
    let feed_id = Uuid::new_v4();
    let feed = Feed {
        id: feed_id,
        url,
        anime: AnimeIdentity { name, season },
        confirmed: true,
        bangumi_info,
    };

    let _ = reply_tx.send(ApiResponse {
        success: true,
        message: feed_id.to_string(),
    });

    let new_state = state.clone().with_feed_added(feed);
    (new_state, vec![])
}

/// API: list all subscribed feeds.
fn reduce_api_list_feeds(
    state: &AppState,
    reply_tx: crossbeam_channel::Sender<Vec<FeedInfo>>,
) -> (AppState, Vec<Effect>) {
    let feeds: Vec<FeedInfo> = state
        .feeds
        .values()
        .map(|f| FeedInfo {
            id: f.id,
            name: f.anime.name.clone(),
            url: f.url.clone(),
            season: f.anime.season,
            bangumi_info: f.bangumi_info.clone(),
        })
        .collect();
    let _ = reply_tx.send(feeds);
    (state.clone(), vec![])
}

/// Executor fetched Bangumi metadata — attach to the feed.
/// API: remove a feed subscription.
fn reduce_api_remove_feed(
    state: &AppState,
    feed_id: Uuid,
    reply_tx: crossbeam_channel::Sender<ApiResponse>,
) -> (AppState, Vec<Effect>) {
    let msg = if state.feeds.contains_key(&feed_id) {
        format!("feed {feed_id} removed")
    } else {
        format!("feed {feed_id} not found")
    };
    let new_state = state.clone().with_feed_removed(feed_id);
    let _ = reply_tx.send(ApiResponse {
        success: true,
        message: msg,
    });
    (new_state, vec![])
}

/// API: return cached download list immediately.
fn reduce_api_list_downloads(
    state: &AppState,
    reply_tx: crossbeam_channel::Sender<Vec<DownloadInfo>>,
) -> (AppState, Vec<Effect>) {
    let _ = reply_tx.send(state.cached_downloads.clone());
    (state.clone(), vec![])
}

/// Trigger a downloader refresh.
fn reduce_refresh_downloads(state: &AppState) -> (AppState, Vec<Effect>) {
    (state.clone(), vec![Effect::QueryAllDownloads])
}

/// Executor sent back fresh snapshots — fill feed context from tracker.
fn reduce_downloads_refreshed(
    state: &AppState,
    snapshots: Vec<DownloadSnapshot>,
) -> (AppState, Vec<Effect>) {
    let downloads: Vec<DownloadInfo> = snapshots
        .into_iter()
        .map(|s| {
            let (feed_name, season) = state
                .tracker
                .get(&s.infohash)
                .map(|r| (r.key.anime.name.clone(), r.key.anime.season))
                .unwrap_or_else(|| (String::new(), 0));
            DownloadInfo {
                infohash: s.infohash,
                feed_name,
                season,
                state: s.state,
                progress: s.progress,
                speed: s.speed,
                size: s.size,
                name: s.name,
            }
        })
        .collect();

    let new_state = state.clone().with_downloads_cached(downloads);
    (new_state, vec![])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::event::DownloadStatus;

    fn empty_state() -> AppState {
        AppState::default()
    }

    #[test]
    fn rss_tick_unknown_feed_returns_empty() {
        let state = empty_state();
        let effects = reduce_rss_tick(&state, Uuid::new_v4());
        assert!(effects.is_empty());
    }

    #[test]
    fn rss_tick_all_empty_state_returns_empty() {
        let state = empty_state();
        let effects = reduce_rss_tick_all(&state);
        assert!(effects.is_empty());
    }

    #[test]
    fn download_failed_returns_empty_effects_and_cleans_state() {
        let state = empty_state();
        let (new_state, effects) =
            reduce_downloader_notification(&state, "abc".into(), DownloadStatus::Failed);
        assert!(effects.is_empty());
        assert_eq!(new_state, state);
    }

    #[test]
    fn download_completed_unknown_gid_returns_empty() {
        let state = empty_state();
        let (_new_state, effects) =
            reduce_downloader_notification(&state, "unknown".into(), DownloadStatus::Completed);
        assert!(effects.is_empty());
    }

    #[test]
    fn confirm_feed_produces_new_state() {
        let state = empty_state();
        let (reply_tx, reply_rx) = crossbeam_channel::bounded(1);
        let (new_state, _effects) = reduce_confirm_feed(
            &state,
            "https://example.com/rss".into(),
            "葬送的芙莉莲".into(),
            2,
            None,
            reply_tx,
        );

        assert_eq!(new_state.feeds.len(), 1);
        let feed = new_state.feeds.values().next().unwrap();
        assert_eq!(feed.anime.name, "葬送的芙莉莲");
        assert_eq!(feed.anime.season, 2);
        assert!(feed.confirmed);
        let resp = reply_rx.try_recv().unwrap();
        assert!(resp.success);
    }

    #[test]
    fn download_started_tracks_in_tracker() {
        let mut state = empty_state();
        let feed_id = Uuid::new_v4();
        let feed = Feed {
            id: feed_id,
            url: "https://example.com/rss".into(),
            anime: AnimeIdentity {
                name: "Oshi no Ko".into(),
                season: 1,
            },
            confirmed: true,
            bangumi_info: None,
        };
        state.feeds.insert(feed_id, feed);

        let (new_state, effects) =
            reduce_download_started(&state, "DEADBEEF".into(), feed_id, String::new());
        assert!(effects.is_empty());
        assert!(new_state.tracker.contains_key("DEADBEEF"));
        let record = new_state.tracker.get("DEADBEEF").unwrap();
        assert_eq!(record.key.anime.name, "Oshi no Ko");
        assert_eq!(record.status, RecordStatus::Downloading);
    }

    #[test]
    fn download_started_dedup_skips_duplicate() {
        let mut state = empty_state();
        let feed_id = Uuid::new_v4();
        let feed = Feed {
            id: feed_id,
            url: "https://example.com/rss".into(),
            anime: AnimeIdentity {
                name: "Oshi no Ko".into(),
                season: 1,
            },
            confirmed: true,
            bangumi_info: None,
        };
        state.feeds.insert(feed_id, feed);

        // First download.
        let (state2, _) =
            reduce_download_started(&state, "DEADBEEF".into(), feed_id, String::new());
        assert_eq!(state2.tracker.len(), 1);

        // Duplicate — should be skipped.
        let (state3, effects) =
            reduce_download_started(&state2, "DEADBEEF".into(), feed_id, String::new());
        assert!(effects.is_empty());
        assert_eq!(state3.tracker.len(), 1);
    }

    #[test]
    fn episode_resolved_updates_tracker() {
        let mut state = empty_state();
        let feed_id = Uuid::new_v4();
        let record = EpisodeRecord {
            infohash: "DEADBEEF".into(),
            torrent_url: String::new(),
            feed_id,
            key: EpisodeKey {
                anime: AnimeIdentity {
                    name: "Oshi no Ko".into(),
                    season: 1,
                },
                episode: 0,
            },
            status: RecordStatus::Downloading,
            library_path: None,
        };
        state.tracker.insert("DEADBEEF".into(), record);

        let (new_state, effects) = reduce_episode_completed(
            &state,
            "DEADBEEF",
            1,
            "/anime/Oshi no Ko/S01/Oshi no Ko S01E01.mkv".into(),
        );

        assert_eq!(effects.len(), 1);
        let r = new_state.tracker.get("DEADBEEF").unwrap();
        assert_eq!(r.key.episode, 1);
        assert_eq!(r.status, RecordStatus::InLibrary);
        assert_eq!(
            r.library_path.as_deref(),
            Some("/anime/Oshi no Ko/S01/Oshi no Ko S01E01.mkv")
        );
    }

    #[test]
    fn downloads_refreshed_uses_tracker() {
        let mut state = empty_state();
        let feed_id = Uuid::new_v4();
        let record = EpisodeRecord {
            infohash: "DEADBEEF".into(),
            torrent_url: String::new(),
            feed_id,
            key: EpisodeKey {
                anime: AnimeIdentity {
                    name: "葬送的芙莉莲".into(),
                    season: 2,
                },
                episode: 38,
            },
            status: RecordStatus::InLibrary,
            library_path: None,
        };
        state.tracker.insert("DEADBEEF".into(), record);

        let snapshots = vec![DownloadSnapshot {
            infohash: "DEADBEEF".into(),
            state: crate::types::DownloadState::Completed,
            progress: 1.0,
            speed: 0,
            size: 500_000_000,
            name: "test".into(),
        }];

        let (new_state, _effects) = reduce_downloads_refreshed(&state, snapshots);
        assert_eq!(new_state.cached_downloads.len(), 1);
        let info = &new_state.cached_downloads[0];
        assert_eq!(info.feed_name, "葬送的芙莉莲");
        assert_eq!(info.season, 2);
    }

    #[test]
    fn confirm_feed_then_rss_tick_includes_it() {
        let state = empty_state();
        let (reply_tx, _reply_rx) = crossbeam_channel::bounded(1);
        let (state2, _) = reduce_confirm_feed(
            &state,
            "https://example.com/rss".into(),
            "Oshi no Ko".into(),
            1,
            None,
            reply_tx,
        );

        // New feed should be included in RSS tick.
        let effects = reduce_rss_tick_all(&state2);
        assert_eq!(effects.len(), 1);
    }
}
