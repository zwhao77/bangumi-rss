//! Filter smoke test — applies a per-feed title filter to a list of titles
//! and reports pass/fail for each, mirroring the RSS fetch-time logic.
//!
//! ## Usage
//!
//! ```bash
//! # Words + regex as CLI flags (include/exclude repeatable)
//! cargo run --bin test-filter -- \
//!   --include suba --include 1080p --exclude sample \
//!   "[SubA] 虚构动画 - 01 [1080P].mp4" \
//!   "[SubB] 虚构动画 - 02 [720P].mp4"
//!
//! # Full FeedFilter from JSON
//! cargo run --bin test-filter -- \
//!   --filter '{"include":["ANi"],"exclude":["sample"],"regex":"1080[Pp]"}'
//!
//! # Read titles from stdin
//! printf '%s\n' "[A] 01" "[A] 02" | \
//!   cargo run --bin test-filter -- --include A
//!
//! # Test directly against an RSS feed (fetches + applies filter to all items)
//! cargo run --bin test-filter -- \
//!   --include nix-raws --exclude sample --url \
//!   "https://mikanani.kas.pub/RSS/Bangumi?bangumiId=3995&subgroupid=1256"
//! ```

use std::io::{self, BufRead, IsTerminal};
use std::process::ExitCode;
use std::time::Duration;

use bangumi_rss::filter;
use bangumi_rss::types::FeedFilter;

fn main() -> ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn"))
        .format_timestamp_millis()
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();

    let mut filter_json: Option<String> = None;
    let mut include: Vec<String> = Vec::new();
    let mut exclude: Vec<String> = Vec::new();
    let mut regex: Option<String> = None;
    let mut url: Option<String> = None;
    let mut titles: Vec<String> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--include" => {
                i += 1;
                include.push(arg_value(&args, i, "--include"));
            }
            "--exclude" => {
                i += 1;
                exclude.push(arg_value(&args, i, "--exclude"));
            }
            "--regex" => {
                i += 1;
                regex = Some(arg_value(&args, i, "--regex"));
            }
            "--filter" => {
                i += 1;
                filter_json = Some(arg_value(&args, i, "--filter"));
            }
            "--url" => {
                i += 1;
                url = Some(arg_value(&args, i, "--url"));
            }
            "-h" | "--help" => {
                print_usage();
                return ExitCode::SUCCESS;
            }
            s if s.starts_with('-') && s.len() > 1 => {
                eprintln!("unknown option: {s}");
                print_usage();
                return ExitCode::from(2);
            }
            s => titles.push(s.to_string()),
        }
        i += 1;
    }

    // Build the filter: --filter JSON wins, otherwise combine flags.
    let feed_filter = match filter_json {
        Some(json) => {
            if !include.is_empty() || !exclude.is_empty() || regex.is_some() {
                eprintln!("--filter cannot be combined with --include/--exclude/--regex");
                return ExitCode::from(2);
            }
            match serde_json::from_str::<FeedFilter>(&json) {
                Ok(f) => f,
                Err(e) => {
                    eprintln!("❌ invalid FeedFilter JSON: {e}");
                    return ExitCode::from(2);
                }
            }
        }
        None => FeedFilter {
            include,
            exclude,
            regex,
        },
    };

    if let Err(e) = filter::validate(&feed_filter) {
        eprintln!("❌ invalid filter: {e}");
        return ExitCode::from(2);
    }

    // Titles come from the RSS URL, positional args, or stdin when piped.
    let feed_titles: Vec<(String, bool)> = match url {
        Some(url) => {
            if !titles.is_empty() {
                eprintln!("--url cannot be combined with positional titles");
                return ExitCode::from(2);
            }
            match fetch_feed_titles(&url) {
                Ok((channel_title, items)) => {
                    println!("channel: {channel_title}");
                    println!("items:   {}", items.len());
                    println!();
                    items
                }
                Err(e) => {
                    eprintln!("❌ fetch/parse failed: {e:#}");
                    return ExitCode::FAILURE;
                }
            }
        }
        None => {
            if titles.is_empty() && !io::stdin().is_terminal() {
                titles = io::stdin()
                    .lock()
                    .lines()
                    .map_while(Result::ok)
                    .filter(|l| !l.trim().is_empty())
                    .collect();
            }
            if titles.is_empty() {
                eprintln!("❌ no titles given (pass as args, pipe to stdin, or use --url)");
                print_usage();
                return ExitCode::from(2);
            }
            titles
                .iter()
                .map(|t| (t.clone(), bangumi_rss::tokenizer::is_batch_title(t)))
                .collect()
        }
    };

    if feed_titles.is_empty() {
        eprintln!("❌ no items found");
        return ExitCode::FAILURE;
    }

    let compiled = match filter::compile(&feed_filter) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("❌ compile failed: {e}");
            return ExitCode::from(2);
        }
    };

    print_filter(&feed_filter);
    println!();

    let mut passed = 0usize;
    for (title, is_batch) in &feed_titles {
        let reason = compiled
            .as_ref()
            .and_then(|c| filter::reject_reason(c, title));
        let ok = reason.is_none();
        if ok {
            passed += 1;
        }
        println!(
            "{}  {title}{}",
            if ok { "✅ PASS" } else { "❌ FAIL" },
            if *is_batch { "  ⚠️ batch" } else { "" }
        );
        if let Some(reason) = reason {
            println!("        reason: {reason}");
        }
    }

    println!();
    println!("{passed}/{} passed", feed_titles.len());
    if passed == feed_titles.len() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn print_filter(f: &FeedFilter) {
    println!("┌──────────────────────────────────┐");
    println!("│  include: {}", join_or_dash(&f.include));
    println!("│  exclude: {}", join_or_dash(&f.exclude));
    println!("│  regex:   {}", f.regex.as_deref().unwrap_or("-"));
    println!("└──────────────────────────────────┘");
}

fn join_or_dash(words: &[String]) -> String {
    if words.is_empty() {
        "-".into()
    } else {
        words.join(", ")
    }
}

/// Fetch an RSS feed and return (channel title, item titles with batch flag).
fn fetch_feed_titles(url: &str) -> anyhow::Result<(String, Vec<(String, bool)>)> {
    let resp = ureq::get(url)
        .set("User-Agent", "ezio/bangumi-rss")
        .timeout(Duration::from_secs(30))
        .call()?;
    let body = resp.into_string()?;
    let channel = body.parse::<rss::Channel>()?;
    let items = channel
        .items()
        .iter()
        .map(|item| {
            let title = item.title().unwrap_or("").to_string();
            (title.clone(), bangumi_rss::tokenizer::is_batch_title(&title))
        })
        .filter(|(t, _)| !t.is_empty())
        .collect();
    Ok((channel.title().to_string(), items))
}

fn arg_value(args: &[String], i: usize, flag: &str) -> String {
    args.get(i)
        .cloned()
        .unwrap_or_else(|| {
            eprintln!("❌ {flag} requires a value");
            std::process::exit(2);
        })
}

fn print_usage() {
    eprintln!(
        "Usage:\n\
         \x20 test-filter [--include WORD]... [--exclude WORD]... [--regex PATTERN] TITLE...\n\
         \x20 test-filter --filter '<FeedFilter JSON>' TITLE...\n\
         \x20 test-filter [--include WORD]... [--exclude WORD]... [--regex PATTERN] --url <RSS-URL>\n\
         \x20 (no titles → read one title per line from stdin)"
    );
}
