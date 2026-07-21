use crossbeam_channel::{Receiver, Sender};
use uuid::Uuid;

use crate::effect::Effect;
use crate::state::AppState;
use crate::traits::RssItem;
use crate::types::BangumiInfo;

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
    EpisodeCompleted {
        infohash: String,
        episode: u32,
        library_path: String,
    },

    /// User confirmed anime name + season via web page.
    UserConfirm {
        feed_id: Uuid,
        name: String,
        season: u8,
    },

    /// API: confirm a feed subscription with resolved anime info.
    ConfirmFeed {
        url: String,
        name: String,
        season: u8,
        #[allow(dead_code)]
        bangumi_info: Option<BangumiInfo>,
        reply_tx: crossbeam_channel::Sender<ApiResponse>,
    },

    /// API: list all feeds.
    ApiListFeeds {
        reply_tx: crossbeam_channel::Sender<Vec<FeedInfo>>,
    },

    /// API: remove a feed subscription.
    ApiRemoveFeed {
        feed_id: Uuid,
        reply_tx: crossbeam_channel::Sender<ApiResponse>,
    },

    /// API: list current downloads (returns cached view immediately).
    ApiListDownloads {
        reply_tx: crossbeam_channel::Sender<Vec<crate::types::DownloadInfo>>,
    },

    /// Trigger a downloader refresh — executor will query and feed back.
    RefreshDownloads,

    /// Executor feedback: fresh download snapshots (logic fills feed names).
    DownloadsRefreshed {
        snapshots: Vec<crate::types::DownloadSnapshot>,
    },
}

#[derive(Debug)]
pub enum DownloadStatus {
    Completed,
    Failed,
}

#[derive(Debug, serde::Serialize)]
pub struct ApiResponse {
    pub success: bool,
    pub message: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct FeedInfo {
    pub id: Uuid,
    pub name: String,
    pub url: String,
    pub season: u8,
    /// Bangumi metadata — present if fetched.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bangumi_info: Option<BangumiInfo>,
}

/// Logic thread entry-point: owns AppState, runs the pure reducer loop.
/// State persistence happens here (only place with &AppState).
pub fn run_logic(event_rx: Receiver<Event>, effect_tx: Sender<Effect>, mut state: AppState) {
    println!("[logic] thread started");

    for event in event_rx {
        let prev = state.clone();
        let (new_state, effects) = crate::logic::reduce(&state, event);
        state = new_state;

        // Persist only when state actually changed.
        if state != prev {
            state.save().ok();
        }

        for e in effects {
            effect_tx.send(e).ok();
        }
    }

    println!("[logic] thread stopped");
}
