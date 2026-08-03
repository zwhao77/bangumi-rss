//! Per-feed torrent title filtering (pure logic).
//!
//! A feed can carry include/exclude regex patterns plus simple substring
//! exclusions. The filter is applied to RSS item titles before a torrent is
//! added, so users can keep same-episode torrents from different groups while
//! excluding unwanted ones.

use regex::Regex;

use crate::types::FeedFilter;

/// Compiled version of [`FeedFilter`] — regexes compiled once per feed tick.
pub struct CompiledFilter {
    include: Vec<Regex>,
    exclude: Vec<Regex>,
    exclude_substrings: Vec<String>,
}

impl CompiledFilter {
    /// Compile a filter. Returns `Ok(None)` when the filter is empty (nothing
    /// to check), `Err` on an invalid regex pattern.
    pub fn compile(filter: &FeedFilter) -> Result<Option<Self>, regex::Error> {
        if filter.include_regex.is_empty()
            && filter.exclude_regex.is_empty()
            && filter.exclude_substrings.is_empty()
        {
            return Ok(None);
        }
        let include = filter
            .include_regex
            .iter()
            .map(|p| Regex::new(p))
            .collect::<Result<Vec<_>, _>>()?;
        let exclude = filter
            .exclude_regex
            .iter()
            .map(|p| Regex::new(p))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Some(Self {
            include,
            exclude,
            exclude_substrings: filter.exclude_substrings.clone(),
        }))
    }

    /// Whether a title passes the filter (true = allowed to download).
    pub fn passes(&self, title: &str) -> bool {
        if !self.include.is_empty() && !self.include.iter().any(|re| re.is_match(title)) {
            return false;
        }
        if self.exclude.iter().any(|re| re.is_match(title)) {
            return false;
        }
        let lower = title.to_lowercase();
        !self
            .exclude_substrings
            .iter()
            .any(|s| lower.contains(&s.to_lowercase()))
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
    fn include_regex_requires_match() {
        let f = FeedFilter {
            include_regex: vec!["SubA".into()],
            ..Default::default()
        };
        assert!(pass(&f, "[SubA] 虚构动画 - 01 [1080P].mp4"));
        assert!(!pass(&f, "[SubB] 虚构动画 - 01 [1080P].mp4"));
    }

    #[test]
    fn include_regex_any_of_matches() {
        let f = FeedFilter {
            include_regex: vec!["SubA".into(), "SubB".into()],
            ..Default::default()
        };
        assert!(pass(&f, "[SubB] 虚构动画 - 01 [1080P].mp4"));
        assert!(!pass(&f, "[SubC] 虚构动画 - 01 [1080P].mp4"));
    }

    #[test]
    fn exclude_regex_skips_match() {
        let f = FeedFilter {
            exclude_regex: vec![r"720[Pp]".into()],
            ..Default::default()
        };
        assert!(pass(&f, "[SubA] 虚构动画 - 01 [1080P].mp4"));
        assert!(!pass(&f, "[SubA] 虚构动画 - 01 [720P].mp4"));
    }

    #[test]
    fn exclude_substring_is_case_insensitive() {
        let f = FeedFilter {
            exclude_substrings: vec!["sample".into()],
            ..Default::default()
        };
        assert!(pass(&f, "[SubA] 虚构动画 - 01 [1080P].mp4"));
        assert!(!pass(&f, "[SubA] 虚构动画 - 01 [1080P].SAMPLE.mp4"));
        assert!(!pass(&f, "[SubA] 虚构动画 - 01 [1080P].Sample.mp4"));
    }

    #[test]
    fn invalid_regex_reports_error() {
        let f = FeedFilter {
            include_regex: vec!["(".into()],
            ..Default::default()
        };
        assert!(CompiledFilter::compile(&f).is_err());
    }
}
