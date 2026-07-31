//! Library crate — single module declaration point for all binaries.
//!
//! `core` is public because `main.rs` (the binary) consumes it directly;
//! `utils` stays crate-private; `tokenizer` is re-exported for dev tools.

pub mod app;
pub mod config;
pub mod core;
pub mod services;
pub mod traits;
pub mod types;
mod utils;

pub use app::run;
pub use utils::tokenizer;
