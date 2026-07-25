//! RSS feed fetcher — preview helper (full parsing is in `utils::rss::parse_rss`).

use crate::types::RssPreview;

/// Concrete RSS client backed by `ureq` + `rss`.
pub struct RssClient;

impl RssClient {
    /// Fetch RSS preview (channel title + up to 5 item titles).
    /// Used by the server for feed confirmation UI.
    pub fn fetch_preview(&self, url: &str) -> anyhow::Result<RssPreview> {
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
    #[ignore = "requires network access to mikanani"]
    fn test_mikan_real_preview() {
        let url = "https://mikanani.kas.pub/RSS/Bangumi?bangumiId=4008&subgroupid=583";
        let client = RssClient;

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
