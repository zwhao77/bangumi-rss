//! Pure utility functions — usable directly from server without TEA pipeline.
//!
//! These combine RSS parsing, tokenizer, and Bangumi API into simple
//! synchronous helpers.  They are **not** behind traits — no mock needed;
//! each sub-component has its own unit tests.

use crate::services::RssClient;
use crate::traits::RssFetcher;
use crate::types::FeedPreview;
use crate::utils::tokenizer;

/// Fetch RSS preview + Bangumi metadata from a feed URL.
///
/// Name extraction: always from item titles via tokenizer.
///   1. Tokenize all item titles → first successful name + season
///   2. Fallback: raw first item title
pub fn fetch_feed_preview(url: &str) -> anyhow::Result<FeedPreview> {
    let rss = RssClient.fetch_preview(url)?;

    // ── Step 1: tokenize item titles ──
    let mut suggested_name = String::new();
    let mut suggested_season: u8 = 1;
    let mut latest_episode: Option<u32> = None;
    let mut group: Option<String> = None;

    for title in &rss.item_titles {
        if let Some(parsed) = tokenizer::parse_torrent_title(title) {
            if suggested_name.is_empty()
                && let Some(n) = parsed.name
            {
                suggested_name = n;
                suggested_season = parsed.season.unwrap_or(1);
            }
            if group.is_none() {
                group = parsed.group;
            }
            if let Some(ep) = parsed.episode {
                let ep_u = ep as u32;
                latest_episode = Some(match latest_episode {
                    Some(cur) if cur > ep_u => cur,
                    _ => ep_u,
                });
            }
        }
    }

    // ── Step 2: raw first item title ──
    let suggested_name = if suggested_name.is_empty() {
        rss.item_titles.first().cloned().unwrap_or_default()
    } else {
        suggested_name
    };

    // ── Bangumi ──
    let mut bangumi_info = if suggested_name.is_empty() {
        None
    } else {
        lookup_bangumi(&suggested_name)
    };

    // Rewrite image URLs to go through backend proxy (lain.bgm.tv blocked in some regions)
    if let Some(ref mut info) = bangumi_info
        && !info.image_url.is_empty()
    {
        let encoded = urlencoding(&info.image_url);
        info.image_url = format!("/api/bangumi/image?url={encoded}");
    }

    Ok(FeedPreview {
        suggested_name,
        suggested_season,
        latest_episode,
        group,
        sample_titles: rss.item_titles,
        bangumi_info,
    })
}

// ── Bangumi lookup ──

fn lookup_bangumi(name: &str) -> Option<crate::types::BangumiInfo> {
    match crate::services::bangumi::search(name) {
        Ok(Some(id)) => match crate::services::bangumi::detail(id) {
            Ok(Some(info)) => {
                println!(
                    "[util] Bangumi detail #{id}: {} · ★{}",
                    info.name_cn,
                    info.rating.map_or("-".into(), |r| format!("{r}"))
                );
                Some(info)
            }
            Ok(None) => {
                println!("[util] Bangumi detail #{id}: no data");
                None
            }
            Err(e) => {
                eprintln!("[util] Bangumi detail #{id} error: {e}");
                None
            }
        },
        Ok(None) => {
            println!("[util] Bangumi search '{name}': not found");
            None
        }
        Err(e) => {
            eprintln!("[util] Bangumi search '{name}' error: {e}");
            None
        }
    }
}

fn urlencoding(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' | b':' => {
                result.push(b as char);
            }
            _ => result.push_str(&format!("%{:02X}", b)),
        }
    }
    result
}
