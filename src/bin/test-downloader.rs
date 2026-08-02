//! Downloader smoke test — connects to a real or mock downloader backend
//! and runs a sequence of operations to verify basic functionality.
//!
//! ## Usage
//!
//! ```bash
//! # Mock (no backend needed, full test including add/remove)
//! MOCK_DOWNLOADER=1 cargo run --bin test-downloader
//!
//! # Real qBittorrent — connect-only (no add unless --torrent-uri given)
//! DOWNLOADER=qbittorrent \
//!   QBITTORRENT_URL=http://localhost:8080 \
//!   QBITTORRENT_USER=admin \
//!   QBITTORRENT_PASS=adminadmin \
//!   cargo run --bin test-downloader
//!
//! # Real qBittorrent — full test with a torrent URI
//! DOWNLOADER=qbittorrent \
//!   QBITTORRENT_URL=http://localhost:8080 \
//!   QBITTORRENT_USER=admin \
//!   QBITTORRENT_PASS=adminadmin \
//!   cargo run --bin test-downloader -- --torrent-uri "magnet:?..."
//! ```

use std::sync::Arc;
use std::time::Instant;

use envconfig::Envconfig;

use bangumi_rss::config::{Config, Downloader};
use bangumi_rss::services;
use bangumi_rss::traits::TorrentDownloader;
use bangumi_rss::types::{CompletedDownload, DownloadSnapshot, TorrentFile};

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp_millis()
        .init();

    let args: Vec<String> = std::env::args().collect();
    let custom_uri = args
        .iter()
        .position(|a| a == "--torrent-uri")
        .and_then(|i| args.get(i + 1).cloned());
    // Concurrent stress mode: N threads hammer the shared downloader.
    let threads = args
        .iter()
        .position(|a| a == "--threads")
        .and_then(|i| args.get(i + 1).cloned())
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(1);
    let iterations = args
        .iter()
        .position(|a| a == "--iterations")
        .and_then(|i| args.get(i + 1).cloned())
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(10);
    // Only test add/remove when a torrent URI is explicitly provided.
    let skip_add = custom_uri.is_none();
    let download_dir = args
        .iter()
        .position(|a| a == "--download-dir")
        .and_then(|i| args.get(i + 1).cloned())
        .unwrap_or_else(|| "/tmp/bangumi-test-dl".to_string());

    std::fs::create_dir_all(&download_dir).ok();

    let config = match Config::init_from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("FATAL: failed to read config from env: {e}");
            std::process::exit(1);
        }
    };

    let dl: Arc<dyn TorrentDownloader> = if config.mock_downloader {
        log::info!("using MockDownloader");
        Arc::new(services::MockDownloader::new())
    } else {
        match config.downloader {
            Downloader::Qbittorrent => {
                log::info!("using QbittorrentDownloader @ {}", config.qbittorrent_url);
                Arc::new(services::QbittorrentDownloader::from_config(
                    config.qbittorrent_url,
                    config.qbittorrent_user,
                    config.qbittorrent_pass,
                ))
            }
            Downloader::Transmission => {
                log::info!(
                    "using TransmissionDownloader @ {}",
                    config.transmission_rpc_url
                );
                Arc::new(services::TransmissionDownloader::with_rpc_url(
                    config.transmission_rpc_url,
                    config.transmission_user,
                    config.transmission_pass,
                ))
            }
            Downloader::Aria2 => {
                log::info!("using Aria2Downloader @ {}", config.aria2_rpc_url);
                Arc::new(services::Aria2Downloader::with_rpc_url(
                    config.aria2_rpc_url,
                    config.aria2_rpc_token,
                ))
            }
        }
    };

    // ── concurrent stress mode (skips the serial suite) ──
    if threads > 1 {
        run_concurrent(&dl, threads, iterations);
        return;
    }

    let mut passed = 0u32;
    let mut failed = 0u32;

    let mut check = |name: &str, result: &dyn std::fmt::Display| {
        print!("  {name:.<40} ");
        let _ = std::io::Write::flush(&mut std::io::stdout());
        println!("{result}");
        passed += 1;
    };

    let mut fail = |name: &str, err: &dyn std::fmt::Display| {
        eprintln!("  {name:.<40} FAIL: {err}");
        failed += 1;
    };

    macro_rules! test {
        ($label:expr, $expr:expr) => {{
            let label = $label;
            match $expr {
                Ok(v) => check(label, &format_args!("{v:?}")),
                Err(e) => fail(label, &e),
            }
        }};
    }

    println!("─── check_connection ───");
    test!("check_connection", dl.check_connection());

    println!("─── query_all (pre) ───");
    test!(
        "query_all(pre)",
        dl.query_all()
            .map(|v: Vec<DownloadSnapshot>| format!("{} items", v.len()))
    );

    if !skip_add {
        println!("─── add torrent ───");
        let uri = custom_uri.as_ref().unwrap();
        let infohash = match dl.add_uri(uri, &download_dir) {
            Ok(h) => {
                check("add_uri", &h);
                h
            }
            Err(e) => {
                fail("add_uri", &e);
                println!("\nSKIP remaining add-dependent tests (no infohash)");
                report(passed, failed);
                return;
            }
        };

        std::thread::sleep(std::time::Duration::from_secs(2));

        println!("─── query_all (post) ───");
        test!(
            "query_all(post)",
            dl.query_all()
                .map(|v: Vec<DownloadSnapshot>| format!("{} items", v.len()))
        );

        println!("─── list_files ───");
        test!(
            "list_files",
            dl.list_files(&infohash)
                .map(|v: Vec<TorrentFile>| format!("{} file(s)", v.len()))
        );

        println!("─── pause / resume ───");
        test!("pause", dl.pause(&infohash));
        std::thread::sleep(std::time::Duration::from_secs(1));
        test!("resume", dl.resume(&infohash));

        println!("─── poll ───");
        test!(
            "poll_completed",
            dl.poll_completed()
                .map(|v: Vec<CompletedDownload>| format!("{} completed", v.len()))
        );
        test!(
            "poll_failed",
            dl.poll_failed()
                .map(|v: Vec<CompletedDownload>| format!("{} failed", v.len()))
        );

        println!("─── cleanup ───");
        test!("remove", dl.remove(&infohash, true));
    }

    report(passed, failed);
}

/// Concurrent stress mode — `threads` threads each run `iterations` ops,
/// rotating through check_connection / query_all / poll_completed to exercise
/// the shared `Arc<dyn TorrentDownloader>` under real parallelism.
fn run_concurrent(dl: &Arc<dyn TorrentDownloader>, threads: usize, iterations: usize) {
    println!("─── concurrent stress: {threads} threads × {iterations} iterations ───");
    let start = Instant::now();
    let mut handles = Vec::new();
    for t in 0..threads {
        let dl = Arc::clone(dl);
        handles.push(std::thread::spawn(move || {
            let (mut ok, mut fail) = (0u32, 0u32);
            for i in 0..iterations {
                let op = (t + i) % 3;
                let r: Result<String, anyhow::Error> = match op {
                    0 => dl.check_connection().map(|()| "connected".to_string()),
                    1 => dl
                        .query_all()
                        .map(|v: Vec<DownloadSnapshot>| format!("{} items", v.len())),
                    _ => dl
                        .poll_completed()
                        .map(|v: Vec<CompletedDownload>| format!("{} completed", v.len())),
                };
                match r {
                    Ok(_) => ok += 1,
                    Err(e) => {
                        fail += 1;
                        log::warn!("[thread {t}] iter {i} op {op} failed: {e}");
                    }
                }
            }
            (ok, fail)
        }));
    }
    let (mut total_ok, mut total_fail) = (0u32, 0u32);
    for h in handles {
        let (ok, fail) = h.join().unwrap();
        total_ok += ok;
        total_fail += fail;
    }
    let elapsed = start.elapsed();
    let total = total_ok + total_fail;
    println!(
        "────── concurrent: {total_ok}/{total} ok, {total_fail} fail, elapsed {elapsed:?} ──────"
    );
    if total_fail > 0 {
        std::process::exit(1);
    }
}

fn report(passed: u32, failed: u32) {
    let total = passed + failed;
    println!(
        "\n────── {passed}/{total} passed{fail_suffix} ──────",
        fail_suffix = if failed > 0 {
            format!(" ({failed} FAILED)")
        } else {
            String::new()
        }
    );
    if failed > 0 {
        std::process::exit(1);
    }
}
