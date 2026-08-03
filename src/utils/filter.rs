//! Per-feed torrent title filtering (pure logic).
//!
//! A feed carries plain whitelist/blacklist words plus an optional advanced
//! Rust regex. Words are matched as case-insensitive substrings — no regex
//! escaping needed for common cases (720p, sample, sub-group names). The
//! filter is applied to RSS item titles before a torrent is added, so users
//! can keep same-episode torrents from different groups while excluding
//! unwanted ones.

use regex::Regex;

use crate::types::FeedFilter;

/// Compiled version of [`FeedFilter`] — regex compiled once per feed tick.
pub struct CompiledFilter {
    /// Lowercased whitelist words.
    include: Vec<String>,
    /// Lowercased blacklist words.
    exclude: Vec<String>,
    /// Optional advanced include regex.
    regex: Option<Regex>,
}

impl CompiledFilter {
    /// Compile a filter. Returns `Ok(None)` when the filter is empty (nothing
    /// to check), `Err` on an invalid regex pattern.
    pub fn compile(filter: &FeedFilter) -> Result<Option<Self>, regex::Error> {
        if filter.include.is_empty() && filter.exclude.is_empty() && filter.regex.is_none() {
            return Ok(None);
        }
        let regex = filter.regex.as_deref().map(Regex::new).transpose()?;
        Ok(Some(Self {
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

    /// Whether a title passes the filter (true = allowed to download).
    ///
    /// Order: include words (all must be present when non-empty) → exclude
    /// words (any hit rejects) → regex (must match when present).
    pub fn passes(&self, title: &str) -> bool {
        let lower = title.to_lowercase();
        if !self.include.is_empty() && !self.include.iter().all(|w| lower.contains(w)) {
            return false;
        }
        if self.exclude.iter().any(|w| lower.contains(w)) {
            return false;
        }
        match &self.regex {
            Some(re) => re.is_match(title),
            None => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pass(filter: &FeedFilter, title: &str) -> bool {
        CompiledFilter::compile(filter)
            .unwrap()
            .map(|c| c.passes(title))
            .unwrap_or(true)
    }

    #[test]
    fn empty_filter_passes_everything() {
        let f = FeedFilter::default();
        assert!(pass(&f, "[SubA] 虚构动画 - 01 [1080P].mp4"));
        assert!(pass(&f, "anything at all"));
    }

    #[test]
    fn include_words_require_all_hits() {
        let f = FeedFilter {
            include: vec!["subA".into(), "subb".into()],
            ..Default::default()
        };
        assert!(pass(&f, "[SubA] 虚构动画 - SubB 01 [1080P].mp4"));
        assert!(!pass(&f, "[SubA] 虚构动画 - 01 [1080P].mp4")); // missing SubB
        assert!(!pass(&f, "[SubC] 虚构动画 - 01 [1080P].mp4"));
    }

    #[test]
    fn exclude_words_skip_case_insensitively() {
        let f = FeedFilter {
            exclude: vec!["720p".into(), "sample".into()],
            ..Default::default()
        };
        assert!(pass(&f, "[SubA] 虚构动画 - 01 [1080P].mp4"));
        assert!(!pass(&f, "[SubA] 虚构动画 - 01 [720P].mp4"));
        assert!(!pass(&f, "[SubA] 虚构动画 - 01 [1080P].SAMPLE.mp4"));
        assert!(!pass(&f, "[SubA] 虚构动画 - 01 [1080P].Sample.mp4"));
    }

    #[test]
    fn regex_is_advanced_include() {
        let f = FeedFilter {
            regex: Some(r"(?i)^\[ANi\].*1080P".into()),
            ..Default::default()
        };
        assert!(pass(&f, "[ANi] 虚构动画 - 01 [1080P].mp4"));
        assert!(!pass(&f, "[SubB] 虚构动画 - 01 [1080P].mp4"));
    }

    #[test]
    fn rules_combine_with_and() {
        let f = FeedFilter {
            include: vec!["subA".into()],
            exclude: vec!["sample".into()],
            regex: Some(r"1080[Pp]".into()),
        };
        assert!(pass(&f, "[SubA] 虚构动画 - 01 [1080P].mp4"));
        assert!(!pass(&f, "[SubB] 虚构动画 - 01 [1080P].mp4")); // include fails
        assert!(!pass(&f, "[SubA] 虚构动画 - 01 SAMPLE [1080P].mp4")); // exclude hits
        assert!(!pass(&f, "[SubA] 虚构动画 - 01 [720P].mp4")); // regex fails
    }

    #[test]
    fn invalid_regex_reports_error() {
        let f = FeedFilter {
            regex: Some("(".into()),
            ..Default::default()
        };
        assert!(CompiledFilter::compile(&f).is_err());
        assert!(CompiledFilter::validate(&f).is_err());
    }

    #[test]
    fn validate_rejects_empty_words() {
        let f = FeedFilter {
            include: vec!["  ".into()],
            ..Default::default()
        };
        assert!(CompiledFilter::validate(&f).is_err());

        let ok = FeedFilter {
            exclude: vec!["720p".into()],
            ..Default::default()
        };
        assert!(CompiledFilter::validate(&ok).is_ok());
    }

    #[test]
    fn words_are_plain_substrings_not_regex() {
        // A word containing regex metacharacters matches literally.
        let f = FeedFilter {
            exclude: vec!["a.b".into()],
            ..Default::default()
        };
        assert!(!pass(&f, "[SubA] a.b 01.mp4"));
        assert!(pass(&f, "[SubA] axb 01.mp4"));
    }
}
