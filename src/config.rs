use envconfig::Envconfig;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Downloader {
    Aria2,
    Qbittorrent,
}

impl std::str::FromStr for Downloader {
    type Err = &'static str;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "aria2" => Ok(Self::Aria2),
            "qbittorrent" => Ok(Self::Qbittorrent),
            _ => Err("expected aria2 or qbittorrent"),
        }
    }
}

#[derive(Envconfig, Debug, PartialEq)]
pub struct Config {
    #[envconfig(from = "PORT", default = "7893")]
    pub port: u16,
    #[envconfig(from = "NO_SERVER", default = "false")]
    pub no_server: bool,
    #[envconfig(from = "DATA_DIR", default = ".")]
    pub data_dir: String,
    #[envconfig(from = "DOWNLOAD_DIR", default = "/downloads")]
    pub download_dir: String,
    #[envconfig(from = "LIBRARY_DIR", default = "/anime")]
    pub library_dir: String,
    #[envconfig(from = "DOWNLOADER", default = "aria2")]
    pub downloader: Downloader,
    #[envconfig(from = "MOCK_DOWNLOADER", default = "false")]
    pub mock_downloader: bool,
    #[envconfig(from = "RSS_INTERVAL", default = "900")]
    pub rss_interval: u64,
    #[envconfig(from = "ARIA2_RPC_URL", default = "http://localhost:6800/jsonrpc")]
    pub aria2_rpc_url: String,
    #[envconfig(from = "QBITTORRENT_URL", default = "http://localhost:8080")]
    pub qbittorrent_url: String,
    #[envconfig(from = "QBITTORRENT_USER", default = "admin")]
    pub qbittorrent_user: String,
    #[envconfig(from = "QBITTORRENT_PASS", default = "adminadmin")]
    pub qbittorrent_pass: String,
    #[envconfig(from = "BANGUMI_API_BASE", default = "https://api.bgm.tv")]
    pub bangumi_api_base: String,
    #[envconfig(from = "MAX_CONCURRENCY", default = "8")]
    pub max_concurrency: usize,
    #[envconfig(from = "TORRENT_CONCURRENCY", default = "4")]
    pub torrent_concurrency: usize,
    #[envconfig(from = "QUEUE_CAPACITY", default = "512")]
    pub queue_capacity: usize,
    #[envconfig(from = "AUTH_USERNAME", default = "")]
    pub auth_username: String,
    #[envconfig(from = "AUTH_PASSWORD", default = "")]
    pub auth_password: String,
}

/// Default HTTP timeout for all outbound requests (RSS, torrent, Aria2 RPC, etc.).
pub const HTTP_TIMEOUT_SECS: u64 = 10;

#[cfg(test)]
mod tests {
    use super::*;
    use envconfig::Envconfig;
    use std::collections::HashMap;

    fn hashmap(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn all_defaults() {
        let c = Config::init_from_hashmap(&hashmap(&[])).unwrap();
        assert_eq!(c.port, 7893);
        assert!(!c.no_server);
        assert_eq!(c.data_dir, ".");
        assert!(matches!(c.downloader, Downloader::Aria2));
        assert_eq!(c.bangumi_api_base, "https://api.bgm.tv");
    }

    #[test]
    fn overrides() {
        let c = Config::init_from_hashmap(&hashmap(&[
            ("PORT", "9999"),
            ("NO_SERVER", "true"),
            ("DOWNLOADER", "qbittorrent"),
        ]))
        .unwrap();
        assert_eq!(c.port, 9999);
        assert!(c.no_server);
        assert!(matches!(c.downloader, Downloader::Qbittorrent));
    }

    #[test]
    fn invalid_downloader() {
        let err =
            Config::init_from_hashmap(&hashmap(&[("DOWNLOADER", "transmission")])).unwrap_err();
        assert!(
            err.to_string().contains("DOWNLOADER"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn optional_dir_not_set() {
        let c = Config::init_from_hashmap(&hashmap(&[])).unwrap();
        assert_eq!(c.data_dir, ".");
        assert_eq!(c.download_dir, "/downloads");
    }

    #[test]
    fn optional_dir_set() {
        let c = Config::init_from_hashmap(&hashmap(&[
            ("DATA_DIR", "/custom/data"),
            ("DOWNLOAD_DIR", "/custom/dl"),
        ]))
        .unwrap();
        assert_eq!(c.data_dir, "/custom/data");
        assert_eq!(c.download_dir, "/custom/dl");
    }
}
