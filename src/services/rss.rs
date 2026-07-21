//! RSS feed fetcher — uses the `rss` crate for robust XML parsing.

use crate::traits::RssFetcher;
use crate::types::{RssItem, RssPreview};

/// Concrete RSS client backed by `ureq` + `rss`.
pub struct RssClient;

impl RssFetcher for RssClient {
    fn fetch(&self, url: &str) -> anyhow::Result<Vec<RssItem>> {
        let body = ureq::get(url).call()?.into_string()?;
        let channel = body.parse::<rss::Channel>()?;
        let mut items = Vec::new();

        for item in channel.items() {
            let title = item.title().unwrap_or("").to_string();
            let torrent_url = item
                .enclosure()
                .map(|e| e.url().to_string())
                .unwrap_or_default();

            if !title.is_empty() && !torrent_url.is_empty() {
                items.push(RssItem { title, torrent_url });
            }
        }
        Ok(items)
    }

    fn fetch_preview(&self, url: &str) -> anyhow::Result<RssPreview> {
        let body = ureq::get(url).call()?.into_string()?;
        let channel = body.parse::<rss::Channel>()?;
        let channel_title = channel.title().to_string();

        let item_titles: Vec<String> = channel
            .items()
            .iter()
            .take(5)
            .filter_map(|i| i.title().map(String::from))
            .collect();

        Ok(RssPreview {
            channel_title,
            item_titles,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mikan_real_rss() {
        let url = "https://mikanani.kas.pub/RSS/Bangumi?bangumiId=4008&subgroupid=583";
        let client = RssClient;

        // Test fetch (full items with torrent URLs).
        let items = client.fetch(url).expect("fetch failed");
        assert!(!items.is_empty(), "should have RSS items");
        for item in &items {
            assert!(!item.title.is_empty(), "title should not be empty");
            assert!(
                item.torrent_url.starts_with("https://") || item.torrent_url.starts_with("magnet:"),
                "unexpected torrent URL: {}",
                item.torrent_url
            );
        }
        println!("fetched {} items", items.len());
        println!("first: {}", items[0].title);

        // Test preview.
        let preview = client.fetch_preview(url).expect("preview failed");
        assert!(!preview.channel_title.is_empty(), "channel title required");
        assert!(!preview.item_titles.is_empty(), "should have sample titles");
        println!("channel: {}", preview.channel_title);
        println!("samples: {:?}", preview.item_titles);

        // Run tokenizer on samples.
        for title in &preview.item_titles {
            if let Some(parsed) = crate::utils::tokenizer::parse_torrent_title(title) {
                println!(
                    "parsed: name={:?} season={:?} ep={:?} group={:?}",
                    parsed.name, parsed.season, parsed.episode, parsed.group
                );
            }
        }
    }
}
