//! Pure post-download logic — extracted from the executor for testability.
//!
//! Takes torrent file metadata and produces effects.  No I/O here.

#[cfg(test)]
use uuid::Uuid;

use crate::types::{AnimeIdentity, EpisodeKey, EpisodeRecord, TorrentFile};
use std::path::PathBuf;

/// Result of scanning a completed torrent's files.
pub(crate) struct ResolvedFile {
    /// Original filename in the torrent, e.g. "[ANi] ... - 01.mp4".
    pub original_name: String,
    /// Episode identity (anime + season + episode number).
    pub key: EpisodeKey,
    /// Normalised output name, e.g. "番剧名 S01E01.mp4".
    pub target_name: String,
    /// Absolute source path (in download staging).
    pub from: PathBuf,
    /// Absolute destination path (in media library).
    pub to: PathBuf,
}

// ── Toolkit ──

/// Extract an `EpisodeKey` from a torrent filename via the tokenizer.
///
/// Returns `None` if the title can't be parsed or the episode is 0.
fn file_to_episode_key(file_name: &str, anime: &AnimeIdentity) -> Option<EpisodeKey> {
    let parsed = crate::utils::tokenizer::parse_torrent_title(file_name)?;
    let episode = parsed.episode.unwrap_or(0.0) as u32;
    if episode == 0 {
        return None;
    }
    Some(EpisodeKey {
        anime: anime.clone(),
        episode,
    })
}

/// True when two keys refer to the same anime *and* episode.
fn episode_keys_match(a: &EpisodeKey, b: &EpisodeKey) -> bool {
    a.anime == b.anime && a.episode == b.episode
}

/// Build a standardised output filename, e.g. `"葬送的芙莉莲 S02E01.mkv"`.
fn key_to_target_name(key: &EpisodeKey, ext: &str) -> String {
    format!(
        "{} S{:02}E{:02}.{ext}",
        key.anime.name, key.anime.season, key.episode
    )
}

/// Build the absolute library path for a resolved file.
fn make_library_path(library_dir: &str, key: &EpisodeKey, target_name: &str) -> PathBuf {
    PathBuf::from(format!(
        "{}/{}/S{:02}/{}",
        library_dir, key.anime.name, key.anime.season, target_name
    ))
}

/// Scan torrent files through the tokenizer and build per‑file metadata.
///
/// `record.key` carries the expected anime identity and episode.  When the
/// expected episode is non‑zero and doesn't match the parsed result, a warning
/// is printed and the expected value is used instead.
pub(crate) fn resolve_files(
    files: &[TorrentFile],
    record: &EpisodeRecord,
    download_dir: &str,
    library_dir: &str,
) -> Vec<ResolvedFile> {
    let expected_key = &record.key;

    let mut resolved: Vec<ResolvedFile> = files
        .iter()
        .filter_map(|f| {
            let key = file_to_episode_key(&f.name, &expected_key.anime)?;
            let mut actual_key = key.clone();

            // Validate against expected.
            if expected_key.episode > 0 && !episode_keys_match(&key, expected_key) {
                log::warn!(
                    "episode mismatch for '{}': file={} expected={}",
                    &f.name[..f.name.len().min(60)],
                    key.episode,
                    expected_key.episode,
                );
                actual_key = expected_key.clone();
            }

            let ext = std::path::Path::new(&f.name)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("mkv");
            let target_name = key_to_target_name(&actual_key, ext);
            let from = PathBuf::from(format!("{}/{}/{}", download_dir, record.feed_id, f.name));
            let to = make_library_path(library_dir, &actual_key, &target_name);

            Some(ResolvedFile {
                original_name: f.name.clone(),
                key: actual_key,
                target_name,
                from,
                to,
            })
        })
        .collect();

    // Multi-file torrents: subsequent files use tokenizer result.
    for f in resolved.iter_mut().skip(1) {
        if let Some(parsed_key) = file_to_episode_key(&f.original_name, &expected_key.anime)
            && !episode_keys_match(&parsed_key, &f.key)
        {
            let ext = std::path::Path::new(&f.original_name)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("mkv");
            f.key = parsed_key.clone();
            f.target_name = key_to_target_name(&parsed_key, ext);
            f.to = make_library_path(library_dir, &parsed_key, &f.target_name);
        }
    }

    resolved
}

/// Resolved file is an internal type — not exposed as a type.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::TorrentFile;

    fn anime() -> AnimeIdentity {
        AnimeIdentity {
            name: "葬送的芙莉莲".into(),
            season: 2,
        }
    }

    #[test]
    fn resolve_matches_episode() {
        let files = vec![
            TorrentFile {
                name: "[ANi] 葬送的芙莉莲 - 01 [1080P].mp4".into(),
            },
            TorrentFile {
                name: "[ANi] 葬送的芙莉莲 - 02 [1080P].mp4".into(),
            },
            TorrentFile {
                name: "not-a-video.txt".into(),
            },
        ];
        let record = EpisodeRecord {
            infohash: "DEADBEEF".into(),
            torrent_url: String::new(),
            feed_id: Uuid::nil(),
            key: EpisodeKey {
                anime: anime(),
                episode: 0,
            },
            status: crate::types::RecordStatus::Downloading,
            library_path: None,
        };
        let resolved = resolve_files(&files, &record, "/downloads", "/anime");
        assert_eq!(resolved.len(), 2);
        assert_eq!(resolved[0].key.episode, 1);
        assert_eq!(resolved[0].target_name, "葬送的芙莉莲 S02E01.mp4");
        assert_eq!(resolved[1].key.episode, 2);
        assert!(resolved[0].from.to_str().unwrap().contains("/downloads/"));
        assert!(
            resolved[0]
                .to
                .to_str()
                .unwrap()
                .contains("/anime/葬送的芙莉莲/S02/")
        );
    }

    #[test]
    fn file_to_episode_key_works() {
        let key = file_to_episode_key("[ANi] 葬送的芙莉莲 - 01 [1080P].mp4", &anime()).unwrap();
        assert_eq!(key.episode, 1);
        assert_eq!(key.anime.name, "葬送的芙莉莲");
    }

    #[test]
    fn episode_keys_match_works() {
        let a = EpisodeKey {
            anime: anime(),
            episode: 1,
        };
        let b = EpisodeKey {
            anime: anime(),
            episode: 1,
        };
        let c = EpisodeKey {
            anime: anime(),
            episode: 2,
        };
        assert!(episode_keys_match(&a, &b));
        assert!(!episode_keys_match(&a, &c));
    }

    #[test]
    fn key_to_target_name_works() {
        let key = EpisodeKey {
            anime: anime(),
            episode: 5,
        };
        assert_eq!(key_to_target_name(&key, "mkv"), "葬送的芙莉莲 S02E05.mkv");
    }

    #[test]
    fn resolve_expected_episode_overrides() {
        let files = vec![TorrentFile {
            name: "[ANi] 葬送的芙莉莲 - 01 [1080P].mp4".into(),
        }];
        let record = EpisodeRecord {
            infohash: "DEADBEEF".into(),
            torrent_url: String::new(),
            feed_id: Uuid::nil(),
            key: EpisodeKey {
                anime: anime(),
                episode: 3,
            },
            status: crate::types::RecordStatus::Downloading,
            library_path: None,
        };
        let resolved = resolve_files(&files, &record, "/dl", "/lib");
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].key.episode, 3);
        assert!(resolved[0].target_name.contains("E03"));
    }

    #[test]
    fn resolve_empty_files() {
        let record = EpisodeRecord {
            infohash: "DEADBEEF".into(),
            torrent_url: String::new(),
            feed_id: Uuid::nil(),
            key: EpisodeKey {
                anime: anime(),
                episode: 0,
            },
            status: crate::types::RecordStatus::Downloading,
            library_path: None,
        };
        let resolved = resolve_files(&[], &record, "/dl", "/lib");
        assert!(resolved.is_empty());
    }

    #[test]
    fn resolve_episode_zero_filtered() {
        let files = vec![TorrentFile {
            name: "[ANi] 葬送的芙莉莲 - 00 [1080P].mp4".into(),
        }];
        let record = EpisodeRecord {
            infohash: "DEADBEEF".into(),
            torrent_url: String::new(),
            feed_id: Uuid::nil(),
            key: EpisodeKey {
                anime: anime(),
                episode: 0,
            },
            status: crate::types::RecordStatus::Downloading,
            library_path: None,
        };
        let resolved = resolve_files(&files, &record, "/dl", "/lib");
        assert!(resolved.is_empty());
    }

    #[test]
    fn resolve_multi_file_second_uses_tokenizer() {
        let files = vec![
            TorrentFile {
                name: "[ANi] 葬送的芙莉莲 - 01 [1080P].mp4".into(),
            },
            TorrentFile {
                name: "[ANi] 葬送的芙莉莲 - 05 [1080P].mp4".into(),
            },
        ];
        let record = EpisodeRecord {
            infohash: "DEADBEEF".into(),
            torrent_url: String::new(),
            feed_id: Uuid::nil(),
            key: EpisodeKey {
                anime: anime(),
                episode: 3,
            },
            status: crate::types::RecordStatus::Downloading,
            library_path: None,
        };
        let resolved = resolve_files(&files, &record, "/dl", "/lib");
        assert_eq!(resolved.len(), 2);
        assert_eq!(resolved[0].key.episode, 3); // overridden by expected
        assert_eq!(resolved[1].key.episode, 5); // tokenizer result
    }
}
