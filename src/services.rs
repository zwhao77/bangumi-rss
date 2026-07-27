//! Service layer — concrete implementations behind `traits::*` interfaces.

pub mod bangumi;
pub mod downloader;
pub mod executor;
pub mod fetch;
pub mod fetch_pool;
pub mod fs;
pub mod mock;
pub mod notify;
pub mod persistence;
pub mod qbittorrent;
mod server;
pub mod timer;

pub use downloader::Aria2Downloader;
pub use executor::EffectExecutor;
pub use fs::RealFileSystem;
pub use mock::{MockDownloader, MockFileSystem};

pub use qbittorrent::QbittorrentDownloader;
pub use server::ServerConfig;
pub use server::start_server;
pub use timer::TimerManager;
