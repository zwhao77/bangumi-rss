use base64::Engine;

use rouille::{Request, Response};

pub fn truncate(s: &str, max: usize) -> String {
    let chars: Vec<_> = s.char_indices().collect();
    if chars.len() <= max {
        return s.to_string();
    }
    let head = max * 3 / 4;
    let tail = max / 4;
    let head_end = chars[head].0;
    let tail_start = chars[chars.len() - tail].0;
    format!(
        "{}...<{} chars omitted>...{}",
        &s[..head_end],
        chars.len().saturating_sub(head + tail),
        &s[tail_start..]
    )
}

pub fn check_auth(request: &Request, username: &str, password: &str) -> bool {
    request
        .header("Authorization")
        .and_then(|h| h.strip_prefix("Basic "))
        .and_then(|encoded| {
            base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .ok()
        })
        .and_then(|decoded| String::from_utf8(decoded).ok())
        .is_some_and(|s| s == format!("{username}:{password}"))
}
pub fn json_response(code: u16, message: &str) -> Response {
    let body = serde_json::json!({"success": false, "message": message}).to_string();
    Response::from_data("application/json", body).with_status_code(code)
}

pub fn is_valid_rss_url(url: &str) -> bool {
    if url.is_empty() || url.len() > 2048 {
        return false;
    }
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return false;
    }
    let after_scheme = &url[url.find("://").unwrap_or(usize::MAX) + 3..];
    after_scheme.starts_with(|c: char| c.is_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn read_body(resp: Response) -> Vec<u8> {
        let (mut reader, _) = resp.data.into_reader_and_size();
        let mut buf = Vec::new();
        reader.read_to_end(&mut buf).unwrap();
        buf
    }

    #[test]
    fn test_json_response() {
        let resp = json_response(404, "not found");
        assert_eq!(resp.status_code, 404);
        let body = read_body(resp);
        let body_str = String::from_utf8(body).unwrap();
        assert!(body_str.contains("not found"));
        assert!(body_str.contains("false"));
    }

    #[test]
    fn test_auth_success() {
        let req = Request::fake_http(
            "GET",
            "/",
            vec![("Authorization".into(), "Basic dXNlcjpwYXNz".into())],
            vec![],
        );
        assert!(check_auth(&req, "user", "pass"));
    }

    #[test]
    fn test_auth_fail_wrong_credentials() {
        let req = Request::fake_http(
            "GET",
            "/",
            vec![("Authorization".into(), "Basic dXNlcjpwYXNz".into())],
            vec![],
        );
        assert!(!check_auth(&req, "user", "wrong"));
    }

    #[test]
    fn test_auth_fail_no_header() {
        let req = Request::fake_http("GET", "/", vec![], vec![]);
        assert!(!check_auth(&req, "user", "pass"));
    }

    #[test]
    fn test_truncate_short() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello", 5), "hello");
    }

    #[test]
    fn test_truncate_long() {
        let s = truncate("abcdefghijklmnopqrstuvwxyz", 10);
        assert!(s.contains("chars omitted"));
        assert!(s.len() > 5);
    }

    #[test]
    fn test_is_valid_rss_url() {
        assert!(is_valid_rss_url("https://example.com/feed.xml"));
        assert!(is_valid_rss_url("http://example.com/feed"));
        assert!(!is_valid_rss_url(""));
        assert!(!is_valid_rss_url("ftp://example.com"));
        assert!(!is_valid_rss_url("not-a-url"));
    }
}
