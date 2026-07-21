//! Service layer — concrete implementations behind `traits::*` interfaces.

pub mod bangumi;
pub mod downloader;
pub mod executor;
pub mod fs;
pub mod mock;
pub mod notify;
pub mod qbittorrent;
pub mod rss;
pub mod server;
pub mod timer;

// Re-export concrete implementations
#[allow(unused_imports)]
pub use downloader::Aria2Downloader;
pub use executor::EffectExecutor;
pub use fs::RealFileSystem;
pub use mock::{MockDownloader, MockRssClient};
pub use notify::NoopNotifier;
pub use qbittorrent::QbittorrentDownloader;
pub use rss::RssClient;
pub use server::start as start_server;
pub use timer::TimerManager;
