//! Service layer — concrete implementations behind `traits::*` interfaces.

pub mod bangumi;
pub mod downloader;
pub mod executor;
pub mod fs;
pub mod mock;
pub mod notify;
pub mod rss;

// Re-export concrete implementations
#[allow(unused_imports)]
pub use bangumi::{BangumiClient, NoopBangumi};
pub use downloader::Aria2Downloader;
pub use executor::EffectExecutor;
pub use fs::RealFileSystem;
pub use mock::{MockDownloader, MockRssClient};
pub use notify::NoopNotifier;
pub use rss::RssClient;
