/// Tokenize an anime torrent title into its components.
///
/// Supports common patterns:
///   [Group] Name 第二季 / English Name - 01 [Tags]
///   [Group]Name - 01 [Tags]
///   [Group] Name - 01v2 [Tags]
#[derive(Debug, Clone)]
pub struct ParsedTitle {
    pub group: Option<String>,
    pub name: Option<String>,    // Chinese / original name
    pub name_jp: Option<String>, // Japanese / romaji name (after "/")
    pub season: Option<u8>,
    pub episode: Option<f32>,
    pub revision: Option<u8>, // v2 revision
}

pub fn parse_torrent_title(raw: &str) -> Option<ParsedTitle> {
    // [Group] or 【Group】
    let group_re = regex::Regex::new(r"^[\[【]([^\]】]+)[\]】]").unwrap();
    let mut rest = raw.to_string();
    let group = group_re.captures(raw).map(|c| {
        rest = rest[c[0].len()..].trim().to_string();
        c[1].to_string()
    });

    // Strip ★...★ metadata blocks (e.g. ★07月新番★)
    let star_re = regex::Regex::new(r"★[^★]+★\s*").unwrap();
    rest = star_re.replace_all(&rest, "").to_string();

    // Split by "/" — left is Chinese, right is Japanese/romaji
    let (left, right) = if let Some(pos) = rest.find(" / ") {
        let l = rest[..pos].trim().to_string();
        let r = rest[pos + 3..].trim().to_string();
        (l, Some(r))
    } else {
        (rest.clone(), None)
    };

    // Season markers
    let season_re = regex::Regex::new(
        r"(?:第([零一二两三四五六七八九十百\d]+)[季期]|(\d)(?:nd|rd|th)?\s*Season|S(\d))\s*",
    )
    .unwrap();
    let mut left_clean = left.clone();
    let season = season_re.captures(&left).and_then(|c| {
        let s = c.get(1).or(c.get(2)).or(c.get(3))?;
        let m = c.get(0).unwrap();
        left_clean = left_clean.replace(m.as_str(), "").trim().to_string();
        parse_cn_number(s.as_str()).or_else(|| s.as_str().parse().ok())
    });

    // Episode number — terminated by ], [, (, or end-of-string.
    // Covers:  - 01 [Tag],  - 01 (1080p),  [38][Tag],  - 01v2
    let ep_re =
        regex::Regex::new(r"(?:[-–—\s]+|\[)(\d+(?:\.\d+)?)(?:v(\d+))?\s*(?:\]|\[|\(|$)").unwrap();
    let (episode, revision) = ep_re
        .captures(&right.clone().unwrap_or(left_clean.clone()))
        .map(|c| {
            let ep: f32 = c[1].parse().unwrap_or(0.0);
            let rev: Option<u8> = c.get(2).and_then(|m| m.as_str().parse().ok());
            (Some(ep), rev)
        })
        .unwrap_or((None, None));

    // Clean name: remove season/episode/tag remnants
    let mut name = strip_tags(&clean_bracket_remnants(&left_clean));
    // If episode was extracted from the name itself (no / split),
    // strip trailing " - NN" or " NN" from the name.
    if right.is_none()
        && let Some(ep) = episode
    {
        let ep_u = ep as u32;
        // Try both padded ("02") and unpadded ("2") forms.
        for suffix in [
            format!(" - {ep_u}"),
            format!(" - {ep_u:02}"),
            format!(" {ep_u}"),
            format!(" {ep_u:02}"),
        ] {
            if let Some(pos) = name.rfind(&suffix) {
                name = name[..pos].to_string();
                break;
            }
        }
    }
    let name = name.trim().to_string();
    let name_jp = right.map(|r| clean_bracket_remnants(&strip_tags(r.trim())));

    Some(ParsedTitle {
        group,
        name: Some(name).filter(|n| !n.is_empty()),
        name_jp,
        season,
        episode,
        revision,
    })
}

fn strip_tags(s: &str) -> String {
    // Remove bracketed tags: [...], 【...】, (...) at end
    let re = regex::Regex::new(r"\[[^\]]+\]|【[^】]+】|\([^)]*\)$").unwrap();
    re.replace_all(s, "").trim().to_string()
}

/// Strip leading `[` and trailing `]` remnants from partial bracket splits.
fn clean_bracket_remnants(s: &str) -> String {
    let s = s.strip_prefix('[').unwrap_or(s);
    let s = s.strip_suffix(']').unwrap_or(s);
    s.trim().to_string()
}

fn parse_cn_number(s: &str) -> Option<u8> {
    match s {
        "零" | "〇" => Some(0),
        "一" => Some(1),
        "二" | "两" => Some(2),
        "三" => Some(3),
        "四" => Some(4),
        "五" => Some(5),
        "六" => Some(6),
        "七" => Some(7),
        "八" => Some(8),
        "九" => Some(9),
        "十" => Some(10),
        other => other.parse().ok(),
    }
}

/// Check if a torrent title looks like a batch release (e.g. "01-12", "01~12").
pub fn is_batch_title(title: &str) -> bool {
    let re = regex::Regex::new(r"\b(\d{1,3})[-~～](\d{1,3})\b").unwrap();
    re.is_match(title)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Standalone extractors (used by tests only) ──

    fn extract_title(raw: &str) -> Option<String> {
        parse_torrent_title(raw).and_then(|p| p.name)
    }

    fn extract_group(raw: &str) -> Option<String> {
        parse_torrent_title(raw).and_then(|p| p.group)
    }

    fn extract_season(raw: &str) -> Option<u8> {
        parse_torrent_title(raw).and_then(|p| p.season)
    }

    fn extract_episode(raw: &str) -> Option<f32> {
        parse_torrent_title(raw).and_then(|p| p.episode)
    }

    // ── Tests ──

    #[test]
    fn test_mikan_style() {
        let p = parse_torrent_title(
            "[绿茶字幕组] 葬送的芙莉莲 第二季 / Sousou no Frieren S2 [38][WebRip][1080p]",
        )
        .unwrap();
        assert_eq!(p.group.as_deref(), Some("绿茶字幕组"));
        assert!(p.name.unwrap().contains("葬送的芙莉莲"));
        assert_eq!(p.season, Some(2));
    }

    #[test]
    fn test_subsplease_style() {
        let p = parse_torrent_title("[SubsPlease] Oshi no Ko - 01 (1080p) [AAC].mkv").unwrap();
        assert_eq!(p.group.as_deref(), Some("SubsPlease"));
        assert_eq!(p.episode, Some(1.0));
    }

    #[test]
    fn test_name_without_episode_suffix() {
        let p = parse_torrent_title(
            "[ANi] 花织即使是转生也想打架 - 02 [1080P][Baha][WEB-DL][AAC AVC][CHT][MP4]",
        )
        .unwrap();
        assert_eq!(p.group.as_deref(), Some("ANi"));
        assert_eq!(p.name.as_deref(), Some("花织即使是转生也想打架"));
        assert_eq!(p.episode, Some(2.0));
    }

    #[test]
    fn test_mikan_fullwidth_group() {
        let p = parse_torrent_title(
            "【喵萌奶茶屋】★07月新番★[相反的你和我 / Seihantai na Kimi to Boku][13][1080p][繁日双语]",
        )
        .unwrap();
        assert_eq!(p.group.as_deref(), Some("喵萌奶茶屋"));
        assert!(p.name.as_deref().unwrap().contains("相反的你和我"));
    }

    #[test]
    fn test_extract_title() {
        assert_eq!(
            extract_title("[ANi] 花织即使是转生也想打架 - 02 [1080P][MP4]").as_deref(),
            Some("花织即使是转生也想打架")
        );
        assert_eq!(
            extract_title("[SubsPlease] Oshi no Ko - 01 (1080p)").as_deref(),
            Some("Oshi no Ko")
        );
    }

    #[test]
    fn test_extract_group() {
        assert_eq!(
            extract_group("[ANi] 花织即使是转生也想打架 - 02 [1080P]").as_deref(),
            Some("ANi")
        );
        assert_eq!(
            extract_group("[SubsPlease] Oshi no Ko - 01 (1080p)").as_deref(),
            Some("SubsPlease")
        );
    }

    #[test]
    fn test_extract_episode() {
        assert_eq!(
            extract_episode("[ANi] 花织即使是转生也想打架 - 02 [1080P]"),
            Some(2.0)
        );
        assert_eq!(
            extract_episode("[SubsPlease] Oshi no Ko - 01 (1080p)"),
            Some(1.0)
        );
    }

    #[test]
    fn test_extract_season() {
        let p = parse_torrent_title(
            "[绿茶字幕组] 葬送的芙莉莲 第二季 / Sousou no Frieren S2 [38][WebRip]",
        )
        .unwrap();
        assert_eq!(p.season, Some(2));
        assert_eq!(
            extract_season("[SomeGroup] Some Anime S3 - 05 [1080P]"),
            Some(3)
        );
    }
}
