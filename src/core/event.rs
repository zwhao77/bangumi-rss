use std::sync::Arc;

use crossbeam_channel::{Receiver, Sender};
use uuid::Uuid;

use crate::core::effect::Effect;
use crate::core::state::AppState;
use crate::services::persistence::save_state;
use crate::traits::FileOps;
use crate::types::{ApiResult, BangumiInfo, FeedInfo, RssItem};

/// Events flow **inward** to the logic thread.
#[derive(Debug)]
pub enum Event {
    /// Periodic refresh for all subscribed feeds.
    RssTickAll,

    /// Executor fetched RSS items → logic decides which to add based on seen_urls.
    RssItemsFetched {
        feed_id: Uuid,
        items: Vec<RssItem>,
        download_dir: String,
    },

    /// Periodic poll from the download watcher — logic emits poll effects.
    PollDownloader,

    /// A download was initiated by the executor (reports infohash).
    DownloadStarted {
        infohash: String,
        feed_id: Uuid,
        torrent_url: String,
    },

    /// A torrent in the downloader changed state.
    DownloaderNotification {
        infohash: String,
        status: DownloadStatus,
    },

    /// Executor resolved episode + moved file to library.
    EpisodeMovedToLibrary {
        infohash: String,
        episode: u32,
        library_path: String,
    },

    /// Executor failed to move files to library (both downloader ops
    /// and filesystem fallback failed).
    EpisodeHandleFailed {
        infohash: String,
    },

    /// User confirmed anime name + season via web page.
    UserConfirm {
        feed_id: Uuid,
        name: String,
        season: u8,
        bangumi_info: Option<BangumiInfo>,
        reply_tx: crossbeam_channel::Sender<ApiResult<String>>,
    },

    /// API: confirm a feed subscription with resolved anime info.
    ConfirmFeed {
        url: String,
        name: String,
        season: u8,
        bangumi_info: Option<BangumiInfo>,
        reply_tx: crossbeam_channel::Sender<ApiResult<String>>,
    },

    /// API: list all feeds.
    ApiListFeeds {
        reply_tx: crossbeam_channel::Sender<ApiResult<Vec<FeedInfo>>>,
    },

    /// API: remove a feed subscription.
    ApiRemoveFeed {
        feed_id: Uuid,
        reply_tx: crossbeam_channel::Sender<ApiResult<String>>,
    },

    /// API: list current downloads (returns cached view immediately).
    ApiListDownloads {
        reply_tx: crossbeam_channel::Sender<ApiResult<Vec<crate::types::DownloadInfo>>>,
    },

    /// Trigger a downloader refresh — executor will query and feed back.
    RefreshDownloads,

    /// Executor feedback: fresh download snapshots (logic fills feed names).
    DownloadsRefreshed {
        snapshots: Vec<crate::types::DownloadSnapshot>,
    },

    /// RSS fetch/parse failed for a feed.
    RssFetchFailed { feed_id: Uuid, error: String },

    /// API: send test notifications to verify webhook config.
    NotifyTest,

    /// API: query a single episode record by infohash (for file serving).
    ApiGetEpisode {
        infohash: String,
        reply_tx: crossbeam_channel::Sender<ApiResult<crate::types::EpisodeRecord>>,
    },
}

#[derive(Debug)]
pub enum DownloadStatus {
    Completed,
    Failed,
}

/// Logic thread entry-point: owns AppState, runs the pure reducer loop.
/// State persistence happens here (only place with &AppState).
pub fn run_logic(
    event_rx: Receiver<Event>,
    effect_tx: Sender<Effect>,
    mut state: AppState,
    fs: Arc<dyn FileOps>,
    data_dir: String,
) {
    log::info!("logic thread started");

    for event in event_rx {
        let prev = state.clone();
        let (new_state, effects) = crate::core::logic::reduce(&state, event);
        state = new_state;

        // Persist only when state actually changed.
        if state != prev {
            save_state(&*fs, &state, &data_dir).ok();
        }

        for e in effects {
            effect_tx.send(e).ok();
        }
    }

    log::info!("logic thread stopped");
}
