//! Service layer — concrete implementations behind `traits::*` interfaces.

pub mod bangumi;
pub mod dl_command;
pub mod downloader;
pub mod executor;
pub mod fetch;
pub mod fetch_pool;
pub mod fs;
pub mod notify;
pub mod persistence;
mod server;
pub mod timer;

pub use downloader::QbittorrentDownloader;
pub use downloader::TransmissionDownloader;
pub use downloader::{Aria2Downloader, MockDownloader, MockFileSystem};
pub use executor::EffectExecutor;
pub use fs::RealFileSystem;
pub use server::ServerConfig;
pub use server::start_server;
pub use timer::TimerManager;
