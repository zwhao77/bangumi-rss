//! Per-feed torrent title filtering (pure logic).
//!
//! A feed carries plain whitelist/blacklist words plus an optional advanced
//! Rust regex. Words are matched as case-insensitive substrings — no regex
//! escaping needed for common cases (720p, sample, sub-group names). The
//! filter is applied to RSS item titles before a torrent is added, so users
//! can keep same-episode torrents from different groups while excluding
//! unwanted ones.
//!
//! Separation of concerns:
//! - [`crate::types::FeedFilter`] is the pure persisted data type (serde only);
//! - [`validate`] checks a filter before it is persisted (create/update);
//! - [`compile`] builds a reusable [`CompiledFilter`] once per RSS tick;
//! - [`reject_reason`] tests one title — returns the first failing rule or
//!   `None`; total, cannot fail once compiled.

use regex::Regex;

use crate::types::FeedFilter;

/// Compiled version of [`FeedFilter`] — regex compiled once per feed tick.
///
/// Opaque handle: build with [`compile`], test with [`reject_reason`].
pub struct CompiledFilter {
    /// Lowercased whitelist words.
    include: Vec<String>,
    /// Lowercased blacklist words.
    exclude: Vec<String>,
    /// Optional advanced include regex.
    regex: Option<Regex>,
}

/// Compile a filter. Returns `Ok(None)` when the filter is empty (nothing to
/// check), `Err` when the regex pattern is invalid.
pub fn compile(filter: &FeedFilter) -> Result<Option<CompiledFilter>, regex::Error> {
    if filter.include.is_empty() && filter.exclude.is_empty() && filter.regex.is_none() {
        return Ok(None);
    }
    let regex = filter.regex.as_deref().map(Regex::new).transpose()?;
    Ok(Some(CompiledFilter {
        include: filter.include.iter().map(|s| s.to_lowercase()).collect(),
        exclude: filter.exclude.iter().map(|s| s.to_lowercase()).collect(),
        regex,
    }))
}

/// Validate a filter before persisting it: reject whitespace-only words
/// (an empty substring would match every title) and invalid regex.
pub fn validate(filter: &FeedFilter) -> Result<(), String> {
    for word in filter.include.iter().chain(filter.exclude.iter()) {
        if word.trim().is_empty() {
            return Err("filter words must not be empty".into());
        }
    }
    if let Some(pattern) = &filter.regex {
        Regex::new(pattern).map_err(|e| format!("invalid regex: {e}"))?;
    }
    Ok(())
}

/// Why a title was rejected by a compiled filter.
///
/// First failing rule wins, in evaluation order: include → exclude → regex.
/// The carried word/pattern is the *first* trigger — e.g. the first missing
/// whitelist word — which is enough to fix a filter against the original
/// title shown in the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RejectReason {
    /// A whitelist word is missing from the title.
    IncludeMissing { word: String },
    /// A blacklist word was found in the title.
    ExcludeMatched { word: String },
    /// The advanced regex did not match (carries the original pattern).
    RegexNoMatch { pattern: String },
}

impl std::fmt::Display for RejectReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::IncludeMissing { word } => write!(f, "missing include word: {word}"),
            Self::ExcludeMatched { word } => write!(f, "excluded word matched: {word}"),
            Self::RegexNoMatch { pattern } => write!(f, "regex did not match: {pattern}"),
        }
    }
}

/// Test one title against a compiled filter.
///
/// Returns the first failing rule with its trigger word/pattern, or `None`
/// when the title passes.
///
/// Total: cannot fail once [`compile`] succeeded — `regex::Regex` reports all
/// errors at compile time; matching is infallible.
///
/// Order: include words (all must be present when non-empty) → exclude words
/// (any hit rejects) → regex (must match when present).
pub fn reject_reason(compiled: &CompiledFilter, title: &str) -> Option<RejectReason> {
    let lower = title.to_lowercase();
    if let Some(word) = compiled.include.iter().find(|w| !lower.contains(*w)) {
        return Some(RejectReason::IncludeMissing { word: word.clone() });
    }
    if let Some(word) = compiled.exclude.iter().find(|w| lower.contains(*w)) {
        return Some(RejectReason::ExcludeMatched { word: word.clone() });
    }
    if let Some(re) = &compiled.regex
        && !re.is_match(title)
    {
        return Some(RejectReason::RegexNoMatch {
            pattern: re.as_str().to_string(),
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reject(filter: &FeedFilter, title: &str) -> Option<RejectReason> {
        compile(filter)
            .unwrap()
            .and_then(|c| reject_reason(&c, title))
    }

    #[test]
    fn empty_filter_passes_everything() {
        let f = FeedFilter::default();
        assert_eq!(reject(&f, "[SubA] 虚构动画 - 01 [1080P].mp4"), None);
        assert_eq!(reject(&f, "anything at all"), None);
    }

    #[test]
    fn include_words_require_all_hits() {
        let f = FeedFilter {
            include: vec!["subA".into(), "subb".into()],
            ..Default::default()
        };
        assert_eq!(reject(&f, "[SubA] 虚构动画 - SubB 01 [1080P].mp4"), None);
        // First missing word in list order (lowercased).
        assert_eq!(
            reject(&f, "[SubA] 虚构动画 - 01 [1080P].mp4"),
            Some(RejectReason::IncludeMissing {
                word: "subb".into()
            })
        );
        assert_eq!(
            reject(&f, "[SubC] 虚构动画 - 01 [1080P].mp4"),
            Some(RejectReason::IncludeMissing {
                word: "suba".into()
            })
        );
    }

    #[test]
    fn exclude_words_skip_case_insensitively() {
        let f = FeedFilter {
            exclude: vec!["720p".into(), "sample".into()],
            ..Default::default()
        };
        assert_eq!(reject(&f, "[SubA] 虚构动画 - 01 [1080P].mp4"), None);
        assert_eq!(
            reject(&f, "[SubA] 虚构动画 - 01 [720P].mp4"),
            Some(RejectReason::ExcludeMatched {
                word: "720p".into()
            })
        );
        assert_eq!(
            reject(&f, "[SubA] 虚构动画 - 01 [1080P].SAMPLE.mp4"),
            Some(RejectReason::ExcludeMatched {
                word: "sample".into()
            })
        );
        assert_eq!(
            reject(&f, "[SubA] 虚构动画 - 01 [1080P].Sample.mp4"),
            Some(RejectReason::ExcludeMatched {
                word: "sample".into()
            })
        );
    }

    #[test]
    fn regex_is_advanced_include() {
        let f = FeedFilter {
            regex: Some(r"(?i)^\[ANi\].*1080P".into()),
            ..Default::default()
        };
        assert_eq!(reject(&f, "[ANi] 虚构动画 - 01 [1080P].mp4"), None);
        assert_eq!(
            reject(&f, "[SubB] 虚构动画 - 01 [1080P].mp4"),
            Some(RejectReason::RegexNoMatch {
                pattern: r"(?i)^\[ANi\].*1080P".into()
            })
        );
    }

    #[test]
    fn rules_combine_with_and() {
        let f = FeedFilter {
            include: vec!["subA".into()],
            exclude: vec!["sample".into()],
            regex: Some(r"1080[Pp]".into()),
        };
        assert_eq!(reject(&f, "[SubA] 虚构动画 - 01 [1080P].mp4"), None);
        assert_eq!(
            reject(&f, "[SubB] 虚构动画 - 01 [1080P].mp4"),
            Some(RejectReason::IncludeMissing {
                word: "suba".into()
            })
        );
        assert_eq!(
            reject(&f, "[SubA] 虚构动画 - 01 SAMPLE [1080P].mp4"),
            Some(RejectReason::ExcludeMatched {
                word: "sample".into()
            })
        );
        assert_eq!(
            reject(&f, "[SubA] 虚构动画 - 01 [720P].mp4"),
            Some(RejectReason::RegexNoMatch {
                pattern: "1080[Pp]".into()
            })
        );
    }

    #[test]
    fn reject_reason_identifies_first_trigger() {
        let f = FeedFilter {
            include: vec!["suba".into(), "1080p".into()],
            exclude: vec!["sample".into()],
            regex: Some(r"(?i)720p".into()),
        };
        // include fails first — first missing word in list order
        assert_eq!(
            reject(&f, "[SubB] 虚构动画 - 01 [1080P].mp4"),
            Some(RejectReason::IncludeMissing {
                word: "suba".into()
            })
        );
        // exclude carries the matched word
        assert_eq!(
            reject(&f, "[SubA] 虚构动画 - 01 SAMPLE [1080P].mp4"),
            Some(RejectReason::ExcludeMatched {
                word: "sample".into()
            })
        );
        // regex carries the original pattern
        assert_eq!(
            reject(&f, "[SubA] 虚构动画 - 01 [1080P].mp4"),
            Some(RejectReason::RegexNoMatch {
                pattern: r"(?i)720p".into()
            })
        );
    }

    #[test]
    fn invalid_regex_reports_error() {
        let f = FeedFilter {
            regex: Some("(".into()),
            ..Default::default()
        };
        assert!(compile(&f).is_err());
        assert!(validate(&f).is_err());
    }

    #[test]
    fn validate_rejects_empty_words() {
        let f = FeedFilter {
            include: vec!["  ".into()],
            ..Default::default()
        };
        assert!(validate(&f).is_err());

        let ok = FeedFilter {
            exclude: vec!["720p".into()],
            ..Default::default()
        };
        assert!(validate(&ok).is_ok());
    }

    #[test]
    fn words_are_plain_substrings_not_regex() {
        // A word containing regex metacharacters matches literally.
        let f = FeedFilter {
            exclude: vec!["a.b".into()],
            ..Default::default()
        };
        assert_eq!(
            reject(&f, "[SubA] a.b 01.mp4"),
            Some(RejectReason::ExcludeMatched { word: "a.b".into() })
        );
        assert_eq!(reject(&f, "[SubA] axb 01.mp4"), None);
    }
}
