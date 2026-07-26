//! Pure RSS XML parsing — no I/O.
//!
//! Parsing functions are pure and easily testable.
//! HTTP fetch is handled by `crate::services::rss`.

use crate::types::{RssItem, RssPreview};
use crate::utils::tokenizer::is_batch_title;

/// Parse an RSS XML body into a list of torrent items.
pub fn parse_rss(body: &str) -> anyhow::Result<Vec<RssItem>> {
    let channel = body.parse::<rss::Channel>()?;
    let mut items = Vec::new();

    for item in channel.items() {
        let title = item.title().unwrap_or("").to_string();
        let torrent_url = item
            .enclosure()
            .map(|e| e.url().to_string())
            .unwrap_or_default();

        if !title.is_empty() && !torrent_url.is_empty() {
            items.push(RssItem {
                is_batch: is_batch_title(&title),
                title,
                torrent_url,
            });
        }
    }
    Ok(items)
}

/// Parse an RSS XML body into a preview (channel title + up to 5 item titles).
/// Pure function, no I/O.
pub fn parse_preview(body: &str) -> anyhow::Result<RssPreview> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_empty_rss() {
        let xml = r#"<?xml version="1.0"?><rss version="2.0"><channel><title>Test</title></channel></rss>"#;
        let items = parse_rss(xml).unwrap();
        assert!(items.is_empty());
    }

    #[test]
    fn parse_single_item() {
        let xml = r#"<?xml version="1.0"?>
<rss version="2.0">
  <channel>
    <title>Anime Feed</title>
    <item>
      <title>[Subs] Anime - 01 [1080p]</title>
      <enclosure url="https://example.com/01.torrent" length="12345" type="application/x-bittorrent"/>
    </item>
  </channel>
</rss>"#;
        let items = parse_rss(xml).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "[Subs] Anime - 01 [1080p]");
        assert_eq!(items[0].torrent_url, "https://example.com/01.torrent");
        assert!(!items[0].is_batch);
    }

    #[test]
    fn parse_skips_item_without_enclosure() {
        let xml = r#"<?xml version="1.0"?>
<rss version="2.0">
  <channel>
    <title>Test</title>
    <item><title>No Enclosure</title></item>
  </channel>
</rss>"#;
        let items = parse_rss(xml).unwrap();
        assert!(items.is_empty());
    }

    #[test]
    fn parse_preview_returns_channel_and_titles() {
        let xml = r#"<?xml version="1.0"?>
<rss version="2.0">
  <channel>
    <title>My Anime Feed</title>
    <item><title>[Subs] Anime - 01 [1080p]</title><enclosure url="https://example.com/01.torrent" length="1" type="application/x-bittorrent"/></item>
    <item><title>[Subs] Anime - 02 [1080p]</title><enclosure url="https://example.com/02.torrent" length="1" type="application/x-bittorrent"/></item>
  </channel>
</rss>"#;
        let preview = parse_preview(xml).unwrap();
        assert_eq!(preview.channel_title, "My Anime Feed");
        assert_eq!(preview.item_titles.len(), 2);
        assert_eq!(preview.item_titles[0], "[Subs] Anime - 01 [1080p]");
    }

    #[test]
    fn parse_preview_limits_to_5_titles() {
        let mut items = String::new();
        for i in 1..=10 {
            items.push_str(&format!(
                r#"<item><title>Ep {i:02}</title><enclosure url="https://example.com/{i:02}.torrent" length="1" type="application/x-bittorrent"/></item>"#,
            ));
        }
        let xml = format!(
            r#"<?xml version="1.0"?><rss version="2.0"><channel><title>Test</title>{items}</channel></rss>"#
        );
        let preview = parse_preview(&xml).unwrap();
        assert_eq!(preview.item_titles.len(), 5);
    }

    #[test]
    fn parse_preview_handles_empty_body() {
        let result = parse_preview("");
        assert!(result.is_err());
    }
}
