//! Bangumi / bgm.tv API client — pure functions.
//!
//! Uses the legacy (no-auth) API:
//! - Search: `GET /search/subject/{keyword}?responseGroup=medium&max_results=N`
//! - Detail: `GET /subject/{id}?responseGroup=large`
//!
//! Proxy: respects `HTTP_PROXY` / `HTTPS_PROXY` env vars (ureq handles this).

use serde::Deserialize;
use std::sync::OnceLock;

use crate::types::BangumiInfo;

const UA: &str = "ezio/bangumi-rss";

static API_BASE: OnceLock<String> = OnceLock::new();

/// Set by main() from Config.bangumi_api_base.
pub fn init_api_base(url: String) {
    let _ = API_BASE.set(url);
}

fn base_url() -> &'static str {
    API_BASE
        .get()
        .map(|s| s.as_str())
        .unwrap_or("https://api.bgm.tv")
}

// ── Response types ──

#[derive(Debug, Deserialize)]
struct BgmSearchResponse {
    list: Option<Vec<BgmSearchItem>>,
}

#[derive(Debug, Deserialize)]
struct BgmSearchItem {
    id: u32,
}

#[derive(Debug, Deserialize)]
struct BgmSubjectResponse {
    #[serde(default)]
    name_cn: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    eps_count: Option<u32>,
    #[serde(default)]
    rating: Option<BgmRating>,
    #[serde(default)]
    air_date: String,
    #[serde(default)]
    images: Option<BgmImages>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    rank: u32,
    #[serde(default)]
    air_weekday: Option<u8>,
}

#[derive(Debug, Deserialize)]
struct BgmRating {
    score: Option<f32>,
    total: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct BgmImages {
    large: Option<String>,
    common: Option<String>,
    medium: Option<String>,
    small: Option<String>,
    grid: Option<String>,
}

impl BgmImages {
    fn best_url(&self) -> &str {
        self.common
            .as_deref()
            .or(self.large.as_deref())
            .or(self.medium.as_deref())
            .or(self.small.as_deref())
            .or(self.grid.as_deref())
            .unwrap_or("")
    }
}

// ── Public API ──

/// Search Bangumi by keyword, return best-match subject ID.
pub fn search(keyword: &str) -> anyhow::Result<Option<u32>> {
    let url = format!(
        "{}/search/subject/{}?responseGroup=medium&max_results=5",
        base_url(),
        url_encode(keyword)
    );
    let resp: BgmSearchResponse = http_get(&url)?.into_json()?;
    Ok(resp
        .list
        .and_then(|items| items.into_iter().next())
        .map(|item| item.id))
}

/// Fetch full metadata for a subject.
pub fn detail(subject_id: u32) -> anyhow::Result<Option<BangumiInfo>> {
    let url = format!("{}/subject/{}?responseGroup=large", base_url(), subject_id);
    let resp: BgmSubjectResponse = http_get(&url)?.into_json()?;

    if resp.error.is_some() || (resp.name_cn.is_empty() && resp.name.is_empty()) {
        return Ok(None);
    }

    let (rating, score_count) = match resp.rating {
        Some(r) => (r.score, r.total),
        None => (None, None),
    };

    let image_url = resp
        .images
        .as_ref()
        .map(|imgs| imgs.best_url())
        .unwrap_or("")
        .to_string();

    Ok(Some(BangumiInfo {
        bangumi_id: subject_id,
        name_cn: resp.name_cn,
        name: resp.name,
        summary: resp.summary.chars().take(200).collect(),
        eps_count: resp.eps_count,
        rating,
        score_count,
        air_date: resp.air_date,
        image_url,
        rank: (resp.rank > 0).then_some(resp.rank),
        air_weekday: resp.air_weekday,
    }))
}

// ── Internal ──

fn http_get(url: &str) -> anyhow::Result<ureq::Response> {
    ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(10))
        .timeout_read(std::time::Duration::from_secs(30))
        .build()
        .get(url)
        .set("User-Agent", UA)
        .call()
        .map_err(|e| e.into())
}

fn url_encode(s: &str) -> String {
    let mut result = String::with_capacity(s.len() * 3);
    for byte in s.as_bytes() {
        match *byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                result.push(*byte as char);
            }
            b' ' => result.push('+'),
            _ => result.push_str(&format!("%{:02X}", byte)),
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_url_encode() {
        assert_eq!(
            url_encode("虚构动画"),
            "%E8%99%9A%E6%9E%84%E5%8A%A8%E7%94%BB"
        );
        assert_eq!(url_encode("hello world"), "hello+world");
        assert_eq!(url_encode("abc-123"), "abc-123");
    }

    #[test]
    fn test_best_url_all_present() {
        let images: BgmImages = serde_json::from_str(
            r#"{
                "large": "http://example.com/l.jpg",
                "common": "http://example.com/c.jpg",
                "medium": "http://example.com/m.jpg",
                "small": "http://example.com/s.jpg",
                "grid": "http://example.com/g.jpg"
            }"#,
        )
        .unwrap();
        assert_eq!(images.best_url(), "http://example.com/c.jpg");
    }

    #[test]
    fn test_best_url_no_common() {
        let images: BgmImages = serde_json::from_str(
            r#"{
                "large": "http://example.com/l.jpg",
                "medium": "http://example.com/m.jpg",
                "small": "http://example.com/s.jpg"
            }"#,
        )
        .unwrap();
        assert_eq!(images.best_url(), "http://example.com/l.jpg");
    }

    #[test]
    fn test_best_url_only_grid() {
        let images: BgmImages =
            serde_json::from_str(r#"{"grid": "http://example.com/g.jpg"}"#).unwrap();
        assert_eq!(images.best_url(), "http://example.com/g.jpg");
    }

    #[test]
    fn test_best_url_empty() {
        let images: BgmImages = serde_json::from_str(r#"{}"#).unwrap();
        assert_eq!(images.best_url(), "");
    }
}
