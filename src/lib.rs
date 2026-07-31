//! Library crate — exposes public types for binaries (main, test-downloader).
//!
//! Internal modules (`core`, `utils`) are kept crate-private; only the
//! public API surface is exported.

pub mod config;
mod core;
pub mod services;
pub mod traits;
pub mod types;
mod utils;
