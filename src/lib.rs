//! Library crate — single module declaration point for all binaries.
//!
//! The public surface is the union of what the three binaries need:
//! - `main` (server): `config::Config` + `run`
//! - `test-downloader` (tool): `config`, `services` (downloader re-exports),
//!   `traits`, `types`
//! - `title-parse` (tool): `tokenizer`
//! - `test-filter` (tool): `filter`, `types` (`FeedFilter`)
//!
//! `app` and `core` stay crate-private (consumed internally via `app::run`);
//! `utils` stays crate-private; `tokenizer` is re-exported for dev tools.

mod app;
pub mod config;
mod core;
pub mod services;
pub mod traits;
pub mod types;
mod utils;

pub use app::run;
pub use utils::filter;
pub use utils::tokenizer;
