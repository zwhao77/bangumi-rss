//! Effect types — pure data that describes a side effect to execute.
//!
//! Effects are produced by the logic layer (`logic::reduce`) and consumed
//! by the service layer (`EffectExecutor` in `services/`).

use std::fmt;
use uuid::Uuid;

use crate::types::AnimeIdentity;

/// An effect to be executed by the service layer.
pub enum Effect {
    /// Fetch and parse an RSS feed, then emit `AddTorrent` for each item.
    FetchRss {
        url: String,
        feed_id: Uuid,
        download_dir: String,
    },

    /// Add a torrent URI to the downloader.
    AddTorrent {
        torrent_url: String,
        save_path: String,
        feed_id: Uuid,
    },

    /// Feed raw .torrent bytes to the downloader (self-call from spawned HTTP fetcher).
    AddTorrentBytes {
        data: Vec<u8>,
        save_path: String,
        feed_id: Uuid,
        torrent_url: String,
    },

    /// Handle a completed download: list files, tokenize, rename, move.
    HandleCompleted {
        infohash: String,
        feed_id: Uuid,
        anime: AnimeIdentity,
        library_dir: String,
        download_dir: String,
        /// Episode number from tracker (0 = not yet known).
        expected_episode: u32,
    },

    /// Send an out-of-band notification (webhook / Server酱).
    Notify { title: String, body: String },

    /// Query the downloader for all current tasks (progress + status).
    QueryAllDownloads,

    /// Poll the downloader for recently completed tasks.
    PollCompleted,

    /// Poll the downloader for recently failed tasks.
    PollFailed,
}

impl fmt::Debug for Effect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FetchRss {
                url,
                feed_id,
                download_dir,
            } => f
                .debug_struct("FetchRss")
                .field("url", url)
                .field("feed_id", feed_id)
                .field("download_dir", download_dir)
                .finish(),
            Self::AddTorrent {
                torrent_url,
                save_path,
                feed_id,
            } => f
                .debug_struct("AddTorrent")
                .field("torrent_url", torrent_url)
                .field("save_path", save_path)
                .field("feed_id", feed_id)
                .finish(),
            Self::AddTorrentBytes {
                data,
                save_path,
                feed_id,
                torrent_url,
            } => f
                .debug_struct("AddTorrentBytes")
                .field("data_len", &data.len())
                .field("save_path", save_path)
                .field("feed_id", feed_id)
                .field("torrent_url", torrent_url)
                .finish(),
            Self::HandleCompleted {
                infohash,
                feed_id,
                anime,
                library_dir,
                download_dir,
                expected_episode,
            } => f
                .debug_struct("HandleCompleted")
                .field("infohash", infohash)
                .field("feed_id", feed_id)
                .field("anime", anime)
                .field("library_dir", library_dir)
                .field("download_dir", download_dir)
                .field("expected_episode", expected_episode)
                .finish(),
            Self::Notify { title, body } => f
                .debug_struct("Notify")
                .field("title", title)
                .field("body", body)
                .finish(),
            Self::QueryAllDownloads => f.debug_struct("QueryAllDownloads").finish(),
            Self::PollCompleted => f.debug_struct("PollCompleted").finish(),
            Self::PollFailed => f.debug_struct("PollFailed").finish(),
        }
    }
}
