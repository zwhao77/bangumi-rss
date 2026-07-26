//! Fetch service — I/O boundary for outbound HTTP requests.
//!
//! Provides two public interfaces that combine HTTP fetch with pure parsing:
//! - [`fetch_items`] — fetch + parse RSS into `Vec<RssItem>` (for background polling)
//! - [`fetch_preview`] — fetch + parse RSS into `RssPreview` (for feed confirmation UI)
//!
//! Also exposes the underlying generic [`fetch_bytes`] for crate-internal use.

use std::io::Read;

use crate::types::{RssItem, RssPreview};

// ── Public API ──

/// Fetch + parse RSS items for torrent download.
pub fn fetch_items(url: &str) -> anyhow::Result<Vec<RssItem>> {
    let body = fetch_rss_body(url)?;
    crate::utils::rss::parse_rss(&body)
}

/// Fetch + parse RSS preview (channel title + up to 5 item titles).
pub fn fetch_preview(url: &str) -> anyhow::Result<RssPreview> {
    let body = fetch_rss_body(url)?;
    crate::utils::rss::parse_preview(&body)
}

// ── Generic HTTP fetch ──

/// Fetch raw bytes from a URL with a timeout and size limit.
pub(crate) fn fetch_bytes(url: &str, timeout: std::time::Duration, max: u64) -> anyhow::Result<Vec<u8>> {
    let resp = ureq::get(url).timeout(timeout).call()?;
    let mut buf = Vec::new();
    resp.into_reader().take(max + 1).read_to_end(&mut buf)?;
    if buf.len() > max as usize {
        anyhow::bail!("response too large: {} bytes (limit {max})", buf.len());
    }
    Ok(buf)
}

// ── RSS helpers ──

/// Fetch RSS body with timeout and 1 MB size limit.
fn fetch_rss_body(url: &str) -> anyhow::Result<String> {
    const MAX: u64 = 1_048_576;
    let timeout = std::time::Duration::from_secs(crate::config::HTTP_TIMEOUT_SECS);
    let bytes = fetch_bytes(url, timeout, MAX)?;
    Ok(String::from_utf8(bytes)?)
}
