//! Downloader implementations — one module per backend, all behind `TorrentDownloader`.

pub mod aria2;
pub mod mock;
pub mod qbittorrent;
pub mod transmission;

pub use aria2::Aria2Downloader;
pub use mock::{MockDownloader, MockFileSystem};
pub use qbittorrent::QbittorrentDownloader;
pub use transmission::TransmissionDownloader;
