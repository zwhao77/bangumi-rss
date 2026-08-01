//! Pure reducer — no I/O, no side effects.
//!
//! Every function takes `&AppState` + an `Event` and returns
//! `(AppState, Vec<Effect>)`.  The caller owns and replaces the state.

use crate::core::effect::Effect;
use crate::core::event::{DownloadStatus, Event};
use crate::core::state::{AppState, Feed};
use std::collections::HashSet;

use crate::types::{
    AnimeIdentity, ApiResult, BangumiInfo, DownloadInfo, DownloadSnapshot, EpisodeDownloadedData,
    EpisodeKey, EpisodeRecord, FailedData, FeedInfo, Notification, RecordStatus, RssItem,
    http_code,
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
        Event::EpisodeMovedToLibrary {
            infohash,
            episode,
            library_path,
        } => reduce_episode_completed(state, &infohash, episode, library_path),
        Event::EpisodeHandleFailed { infohash } => reduce_episode_handle_failed(state, &infohash),
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
        Event::RssFetchFailed { feed_id, error } => reduce_rss_fetch_failed(state, feed_id, error),
        Event::NotifyTest => reduce_notify_test(state),
        Event::ApiGetEpisode { infohash, reply_tx } => {
            reduce_api_get_episode(state, infohash, reply_tx)
        }
        Event::CheckDownloader { reply_tx } => reduce_check_downloader(state, reply_tx),
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
    let mut effects = Vec::new();
    let total = items.len();

    for item in items {
        if item.torrent_url.is_empty() || state.has_url(&item.torrent_url) {
            continue;
        }
        // Reject batch torrents (e.g. "01-12"). Detected during RSS parsing.
        if item.is_batch {
            log::debug!("skip batch: {}", &item.title[..item.title.len().min(80)]);
            continue;
        }
        effects.push(Effect::AddTorrent {
            torrent_url: item.torrent_url,
            save_path: format!("{download_dir}/{feed_id}"),
            feed_id,
        });
    }

    if effects.is_empty() {
        log::debug!(
            "RssItemsFetched: all {} items already seen for feed={feed_id}",
            total
        );
    }

    (state.clone(), effects)
}

/// Periodic download poll → emit poll effects.
fn reduce_poll_downloader() -> Vec<Effect> {
    vec![
        Effect::PollCompleted,
        Effect::PollFailed,
        Effect::QueryAllDownloads,
    ]
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
        torrent_url,
        feed_id,
        key: EpisodeKey {
            anime: feed.anime.clone(),
            episode: 0,
        },
        status: RecordStatus::Downloading,
        library_path: None,
    };

    let new_state = state.clone().with_download_started(record);
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

            // Skip if already handled — prevents loops when downloader
            // re-detects paused/seeding torrents (Transmission/qBittorrent).
            if record.status == RecordStatus::InLibrary || record.status == RecordStatus::Failed {
                log::debug!(
                    "download already in library, skipping: {}",
                    &infohash[..infohash.len().min(16)]
                );
                return (state.clone(), vec![]);
            }

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
            let title = state
                .tracker
                .get(&infohash)
                .map(|r| {
                    let name = &r.key.anime.name;
                    log::warn!(
                        "download failed: {} (infohash={})",
                        name,
                        &infohash[..infohash.len().min(16)]
                    );
                    name.clone()
                })
                .unwrap_or_else(|| {
                    log::warn!(
                        "download failed: unknown (infohash={})",
                        &infohash[..infohash.len().min(16)]
                    );
                    format!("unknown ({})", &infohash[..infohash.len().min(16)])
                });
            let effects = vec![Effect::Notify(Notification::Failed(FailedData {
                title,
                message: "下载失败: 种子已失效或下载器不可用".to_string(),
            }))];
            let mut new_state = state.clone();
            new_state.tracker.remove(&infohash);
            (new_state, effects)
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
            log::warn!("EpisodeMovedToLibrary for unknown infohash: {infohash}");
            return (state.clone(), vec![]);
        }
    };

    // Fetch Bangumi metadata from the feed if available.
    let (name_cn, name_original, summary, rating, image_url, eps_count) = state
        .feeds
        .get(&record.feed_id)
        .and_then(|f| f.bangumi_info.as_ref())
        .map(|b| {
            (
                Some(b.name_cn.clone()),
                Some(b.name.clone()),
                Some(b.summary.clone()),
                b.rating,
                Some(b.image_url.clone()),
                b.eps_count,
            )
        })
        .unwrap_or((None, None, None, None, None, None));

    let notification = Notification::EpisodeDownloaded(EpisodeDownloadedData {
        anime_name: record.key.anime.name.clone(),
        season: record.key.anime.season,
        episode,
        library_path: library_path.clone(),
        name_cn,
        name_original,
        summary,
        rating,
        image_url,
        eps_count,
    });

    let effects = vec![Effect::Notify(notification)];

    let new_state = state
        .clone()
        .with_episode_resolved(infohash, episode, library_path)
        .with_download_in_library(infohash);

    // Place effects after state update so save happens first.
    (new_state, effects)
}

/// Executor failed to move files to library — mark as Failed.
fn reduce_episode_handle_failed(state: &AppState, infohash: &str) -> (AppState, Vec<Effect>) {
    if !state.tracker.contains_key(infohash) {
        log::warn!("EpisodeHandleFailed for unknown infohash: {infohash}");
        return (state.clone(), vec![]);
    }

    log::warn!("episode handle failed, marking as Failed: {infohash}");
    let new_state = state.clone().with_download_failed(infohash);
    (new_state, vec![])
}

/// User confirmed the anime name + season via the web UI.
fn reduce_user_confirm(
    state: &AppState,
    feed_id: Uuid,
    name: String,
    season: u8,
    bangumi_info: Option<BangumiInfo>,
    reply_tx: crossbeam_channel::Sender<ApiResult<String>>,
) -> (AppState, Vec<Effect>) {
    let exists = state.feeds.contains_key(&feed_id);
    let new_state = state
        .clone()
        .with_feed_confirmed(feed_id, name, season, bangumi_info);
    if exists {
        let _ = reply_tx.send(ApiResult::OK {
            value: "updated".into(),
        });
    } else {
        let _ = reply_tx.send(ApiResult::Err {
            code: http_code::NOT_FOUND,
            message: format!("feed {feed_id} not found"),
        });
    }
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
    reply_tx: crossbeam_channel::Sender<ApiResult<String>>,
) -> (AppState, Vec<Effect>) {
    if name.trim().is_empty() {
        let _ = reply_tx.send(ApiResult::Err {
            code: http_code::BAD_REQUEST,
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

    let _ = reply_tx.send(ApiResult::OK {
        value: feed_id.to_string(),
    });

    let new_state = state.clone().with_feed_added(feed);
    (new_state, vec![])
}

/// API: list all subscribed feeds.
fn reduce_api_list_feeds(
    state: &AppState,
    reply_tx: crossbeam_channel::Sender<ApiResult<Vec<FeedInfo>>>,
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
    let _ = reply_tx.send(ApiResult::OK { value: feeds });
    (state.clone(), vec![])
}

/// Executor fetched Bangumi metadata — attach to the feed.
/// API: remove a feed subscription.
fn reduce_api_remove_feed(
    state: &AppState,
    feed_id: Uuid,
    reply_tx: crossbeam_channel::Sender<ApiResult<String>>,
) -> (AppState, Vec<Effect>) {
    if !state.feeds.contains_key(&feed_id) {
        let _ = reply_tx.send(ApiResult::Err {
            code: http_code::NOT_FOUND,
            message: format!("feed {feed_id} not found"),
        });
        return (state.clone(), vec![]);
    }
    let new_state = state.clone().with_feed_removed(feed_id);
    let _ = reply_tx.send(ApiResult::OK {
        value: format!("feed {feed_id} removed"),
    });
    (new_state, vec![])
}

/// API: return cached download list immediately.
fn reduce_api_list_downloads(
    state: &AppState,
    reply_tx: crossbeam_channel::Sender<ApiResult<Vec<DownloadInfo>>>,
) -> (AppState, Vec<Effect>) {
    let _ = reply_tx.send(ApiResult::OK {
        value: state.cached_downloads.clone(),
    });
    (state.clone(), vec![])
}

/// Trigger a downloader refresh.
fn reduce_refresh_downloads(state: &AppState) -> (AppState, Vec<Effect>) {
    (state.clone(), vec![Effect::QueryAllDownloads])
}

/// Health check — forward to the executor, which probes the downloader.
fn reduce_check_downloader(
    state: &AppState,
    reply_tx: crossbeam_channel::Sender<ApiResult<()>>,
) -> (AppState, Vec<Effect>) {
    (state.clone(), vec![Effect::CheckDownloader { reply_tx }])
}

/// RSS fetch/parse failed — log and notify.
fn reduce_rss_fetch_failed(
    state: &AppState,
    feed_id: Uuid,
    error: String,
) -> (AppState, Vec<Effect>) {
    log::warn!("RSS fetch failed for feed={feed_id}: {error}");
    let url = state
        .feeds
        .get(&feed_id)
        .map(|f| f.url.as_str())
        .unwrap_or("unknown");
    let effects = vec![Effect::Notify(Notification::Failed(FailedData {
        title: url.into(),
        message: format!("RSS 获取失败: {error}"),
    }))];
    (state.clone(), effects)
}

/// API: send two test notifications to verify webhook config.
fn reduce_notify_test(state: &AppState) -> (AppState, Vec<Effect>) {
    let effects = vec![
        Effect::Notify(Notification::EpisodeDownloaded(EpisodeDownloadedData {
            anime_name: "测试通知".into(),
            season: 1,
            episode: 1,
            library_path: "/anime/测试通知/S01/E01.mp4".into(),
            image_url: None,
            name_cn: None,
            name_original: None,
            summary: Some("这是一条测试消息，用于验证通知配置".into()),
            rating: None,
            eps_count: None,
        })),
        Effect::Notify(Notification::Failed(FailedData {
            title: "测试通知".into(),
            message: "这是一条模拟错误，用于验证错误通知配置".into(),
        })),
    ];
    (state.clone(), effects)
}

/// Executor sent back fresh snapshots — fill feed context from tracker.
fn reduce_downloads_refreshed(
    state: &AppState,
    snapshots: Vec<DownloadSnapshot>,
) -> (AppState, Vec<Effect>) {
    let known: HashSet<&str> = snapshots.iter().map(|s| s.infohash.as_str()).collect();

    let effects: Vec<Effect> = Vec::new();
    let mut new_state = state.clone();
    let mut vanished_count = 0u32;

    // Cross-reference: find tracker entries that vanished from aria2 (restart/removed)
    // Remove both tracker entry and seen_urls so RSS poll can re-download.
    for (ih, record) in &state.tracker {
        if record.status != RecordStatus::Downloading {
            continue;
        }
        if known.contains(ih.as_str()) {
            continue;
        }
        vanished_count += 1;
        log::warn!(
            "task vanished from aria2: {} (infohash={})",
            record.key.anime.name,
            &ih[..ih.len().min(16)]
        );
        if !record.torrent_url.is_empty() {
            new_state = new_state.with_seen_url_removed(&record.torrent_url);
        }
        new_state = new_state.with_tracker_removed(ih);
    }
    if vanished_count > 0 {
        log::info!(
            "reconciliation: {} snapshot(s) from aria2, {} tracker entry(ies), {} vanished → removed + seen_urls cleaned",
            known.len(),
            state.tracker.len(),
            vanished_count,
        );
    }

    // Cross-reference: detect duplicate downloads (same infohash, different status)
    for (ih, record) in &state.tracker {
        if record.status != RecordStatus::InLibrary {
            continue;
        }
        // Check if another entry with the same infohash is still Downloading
        if let Some(other) = state.tracker.get(ih)
            && other.status == RecordStatus::Downloading
            && other.infohash == record.infohash
        {
            log::warn!(
                "duplicate download detected: infohash={} is both InLibrary and Downloading",
                &ih[..ih.len().min(16)]
            );
        }
    }

    // Build cached download list as before
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

    new_state = new_state.with_downloads_cached(downloads);
    (new_state, effects)
}

/// API: query a single episode record by infohash (used for file serving).
fn reduce_api_get_episode(
    state: &AppState,
    infohash: String,
    reply_tx: crossbeam_channel::Sender<ApiResult<crate::types::EpisodeRecord>>,
) -> (AppState, Vec<Effect>) {
    match state.tracker.get(&infohash) {
        Some(record) => {
            let _ = reply_tx.send(ApiResult::OK {
                value: record.clone(),
            });
        }
        None => {
            let _ = reply_tx.send(ApiResult::Err {
                code: http_code::NOT_FOUND,
                message: "not found".into(),
            });
        }
    }
    (state.clone(), vec![])
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
        assert_eq!(effects.len(), 1);
        assert!(matches!(effects[0], Effect::Notify(_)));
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
            "星海物语".into(),
            2,
            None,
            reply_tx,
        );

        assert_eq!(new_state.feeds.len(), 1);
        let feed = new_state.feeds.values().next().unwrap();
        assert_eq!(feed.anime.name, "星海物语");
        assert_eq!(feed.anime.season, 2);
        assert!(feed.confirmed);
        let resp = reply_rx.try_recv().unwrap();
        assert!(matches!(resp, ApiResult::OK { .. }));
    }

    #[test]
    fn download_started_tracks_in_tracker() {
        let mut state = empty_state();
        let feed_id = Uuid::new_v4();
        let feed = Feed {
            id: feed_id,
            url: "https://example.com/rss".into(),
            anime: AnimeIdentity {
                name: "星海物语".into(),
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
        assert_eq!(record.key.anime.name, "星海物语");
        assert_eq!(record.status, RecordStatus::Downloading);
    }

    #[test]
    fn download_started_inserts_seen_url() {
        let mut state = empty_state();
        let feed_id = Uuid::new_v4();
        let feed = Feed {
            id: feed_id,
            url: "https://example.com/rss".into(),
            anime: AnimeIdentity {
                name: "星海物语".into(),
                season: 1,
            },
            confirmed: true,
            bangumi_info: None,
        };
        state.feeds.insert(feed_id, feed);

        let url = "https://example.com/download/anime.torrent";
        let (new_state, effects) =
            reduce_download_started(&state, "DEADBEEF".into(), feed_id, url.into());
        assert!(effects.is_empty());
        assert!(
            new_state.seen_urls.contains(url),
            "seen_urls should contain the torrent URL"
        );
        let record = new_state.tracker.get("DEADBEEF").unwrap();
        assert_eq!(record.torrent_url, url);
    }

    #[test]
    fn download_started_dedup_skips_duplicate() {
        let mut state = empty_state();
        let feed_id = Uuid::new_v4();
        let feed = Feed {
            id: feed_id,
            url: "https://example.com/rss".into(),
            anime: AnimeIdentity {
                name: "星海物语".into(),
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
                    name: "星海物语".into(),
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
            "/anime/星海物语/S01/星海物语 S01E01.mkv".into(),
        );

        assert_eq!(effects.len(), 1);
        let r = new_state.tracker.get("DEADBEEF").unwrap();
        assert_eq!(r.key.episode, 1);
        assert_eq!(r.status, RecordStatus::InLibrary);
        assert_eq!(
            r.library_path.as_deref(),
            Some("/anime/星海物语/S01/星海物语 S01E01.mkv")
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
                    name: "星海物语".into(),
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
        assert_eq!(info.feed_name, "星海物语");
        assert_eq!(info.season, 2);
    }

    #[test]
    fn confirm_feed_then_rss_tick_includes_it() {
        let state = empty_state();
        let (reply_tx, _reply_rx) = crossbeam_channel::bounded(1);
        let (state2, _) = reduce_confirm_feed(
            &state,
            "https://example.com/rss".into(),
            "星海物语".into(),
            1,
            None,
            reply_tx,
        );

        // New feed should be included in RSS tick.
        let effects = reduce_rss_tick_all(&state2);
        assert_eq!(effects.len(), 1);
    }

    #[test]
    fn rss_fetch_failed_returns_unchanged_state() {
        let state = empty_state();
        let (new_state, effects) =
            reduce_rss_fetch_failed(&state, uuid::Uuid::new_v4(), "connection timeout".into());
        assert_eq!(effects.len(), 1);
        assert!(matches!(effects[0], Effect::Notify(_)));
        assert_eq!(new_state, state);
    }
}
