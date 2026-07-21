//! Pure utility functions — usable directly from server without TEA pipeline.
//!
//! These combine RSS parsing, tokenizer, and Bangumi API into simple
//! synchronous helpers.  They are **not** behind traits — no mock needed;
//! each sub-component has its own unit tests.

use crate::services::{BangumiClient, RssClient};
use crate::tokenizer;
use crate::traits::{BangumiSearcher, RssFetcher};
use crate::types::FeedPreview;

/// Fetch RSS preview + Bangumi metadata from a feed URL.
///
/// 1. Parse the RSS feed → channel title + sample item titles.
/// 2. Tokenize titles → suggested name, season, episode number, group.
/// 3. Search Bangumi by suggested name → subject ID → full detail.
///
/// Return a `FeedPreview` ready for the web UI, with optional `bangumi_info`.
pub fn fetch_feed_preview(url: &str) -> anyhow::Result<FeedPreview> {
    // ── RSS ──
    let rss = RssClient.fetch_preview(url)?;

    // ── Tokenize ──
    let mut suggested_name = String::new();
    let mut suggested_season: u8 = 1;
    let mut latest_episode: Option<u32> = None;
    let mut group: Option<String> = None;

    for title in &rss.item_titles {
        if suggested_name.is_empty() {
            suggested_name = tokenizer::extract_title(title).unwrap_or_default();
        }
        if group.is_none() {
            group = tokenizer::extract_group(title);
        }
        if let Some(s) = tokenizer::extract_season(title) {
            suggested_season = s;
        }
        if let Some(ep) = tokenizer::extract_episode(title) {
            let ep_u = ep as u32;
            latest_episode = Some(match latest_episode {
                Some(cur) if cur > ep_u => cur,
                _ => ep_u,
            });
        }
    }

    // Fallback to channel title if tokenizer couldn't extract a name.
    if suggested_name.is_empty() {
        suggested_name = rss.channel_title;
    }

    // ── Bangumi (use tokenized name, not channel title) ──
    let bangumi_info = match BangumiClient.search_subject_id(&suggested_name) {
        Ok(Some(id)) => {
            println!("[util] Bangumi search '{suggested_name}' → subject #{id}");
            match BangumiClient.get_subject_detail(id) {
                Ok(Some(info)) => {
                    println!(
                        "[util] Bangumi detail #{id}: {} · {} eps · ★{}",
                        info.name_cn,
                        info.eps_count.map_or("-".into(), |e| e.to_string()),
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
            }
        }
        Ok(None) => {
            println!("[util] Bangumi search '{suggested_name}': not found");
            None
        }
        Err(e) => {
            eprintln!("[util] Bangumi search '{suggested_name}' error: {e}");
            None
        }
    };

    Ok(FeedPreview {
        suggested_name,
        suggested_season,
        latest_episode,
        group,
        sample_titles: rss.item_titles,
        bangumi_info,
    })
}
