//! Pure utility functions — no I/O, no traits, easily testable.
//!
//! Organized into submodules by domain:
//! - `tokenizer` — torrent title parsing
//! - `handler` — post-download file resolution
//! - `preview` — RSS + Bangumi preview helper

pub mod handler;
pub mod preview;
pub mod tokenizer;
