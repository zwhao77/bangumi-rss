use base64::Engine;

use serde::Serialize;

use rouille::{Request, Response};

use crate::types::{http_code, problem_type, ApiResult, Problem};

/// Media type for RFC 9457 Problem Details responses.
pub const PROBLEM_JSON: &str = "application/problem+json";

/// Constant-time string equality — timing does not reveal how many leading
/// bytes matched (see API.md §2).
pub fn constant_time_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.bytes().zip(b.bytes()) {
        diff |= x ^ y;
    }
    diff == 0
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
        .is_some_and(|s| constant_time_eq(&s, &format!("{username}:{password}")))
}

/// Success envelope: `{"data": <T>}` (API.md §3).
pub fn json_data<T: Serialize>(status: u16, value: &T) -> Response {
    let body = serde_json::json!({"data": value}).to_string();
    Response::from_data("application/json", body).with_status_code(status)
}

/// RFC 9457 Problem Details response (API.md §3/§6).
pub fn problem_response(status: u16, type_uri: &str, title: &str, detail: &str) -> Response {
    let problem = Problem {
        r#type: type_uri.into(),
        title: title.into(),
        status,
        detail: detail.into(),
        instance: None,
    };
    let body = serde_json::to_string(&problem).unwrap_or_default();
    Response::from_data(PROBLEM_JSON, body).with_status_code(status)
}

/// Map an HTTP status to its registry problem type + title (API.md §6).
pub fn problem_type_for_status(code: u16) -> (&'static str, &'static str) {
    match code {
        http_code::BAD_REQUEST => (problem_type::INVALID_REQUEST, "Invalid request"),
        http_code::NOT_FOUND => (problem_type::NOT_FOUND, "Not found"),
        http_code::SERVICE_UNAVAILABLE => {
            (problem_type::SERVICE_UNAVAILABLE, "Service unavailable")
        }
        http_code::INTERNAL => (problem_type::INTERNAL, "Internal error"),
        _ => (problem_type::INTERNAL, "Internal error"),
    }
}

/// RFC 9457 Problem Details response for an `ApiResult::Err` code/message.
///
/// Filter-validation failures (produced by the logic layer) get the more
/// specific `invalid-filter` problem type.
pub fn api_err_response(code: u16, message: &str) -> Response {
    if code == http_code::BAD_REQUEST && message.starts_with("invalid filter") {
        return problem_response(
            code,
            problem_type::INVALID_FILTER,
            "Invalid feed filter",
            message,
        );
    }
    let (type_uri, title) = problem_type_for_status(code);
    problem_response(code, type_uri, title, message)
}

/// Map an internal `ApiResult` to a contract response: `OK` → `{"data": value}`
/// with `ok_status`; `Err` → RFC 9457 Problem Details. The single conversion
/// point for all `ApiResult`-backed JSON endpoints (fire-and-forget actions
/// build their own `202` body — see API.md §3).
pub fn api_result_response<T: Serialize>(result: ApiResult<T>, ok_status: u16) -> Response {
    match result {
        ApiResult::OK { value } => json_data(ok_status, &value),
        ApiResult::Err { code, message } => api_err_response(code, &message),
    }
}

/// Known routes as `(path segments, allowed methods)`. `"{}"` matches any
/// single segment. Used to answer `405` before rouille's router (which would
/// otherwise fall through to 404).
const ROUTES: &[(&[&str], &[&str])] = &[
    (&[], &["GET"]), // GET /
    (&["style.css"], &["GET"]),
    (&["api", "feeds"], &["GET", "POST"]),
    (&["api", "feeds", "preview"], &["POST"]),
    (&["api", "feeds", "refresh"], &["POST"]),
    (&["api", "feeds", "{}"], &["PUT", "DELETE"]),
    (&["api", "files", "{}"], &["GET"]),
    (&["api", "downloads"], &["GET"]),
    (&["api", "downloads", "refresh"], &["POST"]),
    (&["api", "downloads", "poll"], &["POST"]),
    (&["api", "bangumi", "subjects", "{}"], &["GET"]),
    (&["api", "bangumi", "search"], &["GET"]),
    (&["api", "health"], &["GET"]),
    (&["api", "notify", "test"], &["POST"]),
];

/// If `path` matches a known route but `method` is not allowed, return a
/// `405` Problem Details response with an `Allow` header.
pub fn method_not_allowed_response(method: &str, path: &str) -> Option<Response> {
    let path = path.split('?').next().unwrap_or(path);
    let segments: Vec<&str> = path
        .trim_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();

    for &(pattern, methods) in ROUTES {
        if segments_match(pattern, &segments) {
            if methods.contains(&method) {
                // Route exists and the method is allowed — let the router handle it.
                return None;
            }
            let allow = methods.join(", ");
            let mut resp = problem_response(
                http_code::METHOD_NOT_ALLOWED,
                problem_type::METHOD_NOT_ALLOWED,
                "Method not allowed",
                &format!("{method} is not allowed for {path}"),
            );
            resp.headers.push(("Allow".into(), allow.into()));
            return Some(resp);
        }
    }
    None
}

fn segments_match(pattern: &[&str], segments: &[&str]) -> bool {
    pattern.len() == segments.len()
        && pattern
            .iter()
            .zip(segments)
            .all(|(p, s)| *p == "{}" || p == s)
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
    fn test_problem_response() {
        let resp = problem_response(404, problem_type::NOT_FOUND, "Not found", "no such feed");
        assert_eq!(resp.status_code, 404);
        let body = read_body(resp);
        let body_str = String::from_utf8(body).unwrap();
        assert!(body_str.contains("\"type\":\"urn:bangumi-rss:problems:not-found\""));
        assert!(body_str.contains("\"status\":404"));
        assert!(body_str.contains("no such feed"));
        assert!(!body_str.contains("success"));
    }

    #[test]
    fn test_constant_time_eq() {
        assert!(constant_time_eq("user:pass", "user:pass"));
        assert!(!constant_time_eq("user:pass", "user:passw"));
        assert!(!constant_time_eq("user:pass", "user:pasx"));
        assert!(!constant_time_eq("aaaa", "zzzz"));
        assert!(!constant_time_eq("", "x"));
        assert!(constant_time_eq("", ""));
    }

    #[test]
    fn test_method_not_allowed() {
        // Wrong method on a known route → 405 with Allow.
        let resp = method_not_allowed_response("DELETE", "/api/feeds").unwrap();
        assert_eq!(resp.status_code, 405);
        assert!(resp
            .headers
            .iter()
            .any(|(k, v)| k == "Allow" && v == "GET, POST"));

        // Allowed method → no 405.
        assert!(method_not_allowed_response("GET", "/api/feeds").is_none());
        assert!(method_not_allowed_response("POST", "/api/feeds").is_none());
        assert!(method_not_allowed_response("PUT", "/api/feeds/abc").is_none());
        assert!(method_not_allowed_response("GET", "/").is_none());

        // Unknown path → no 405 (router will 404).
        assert!(method_not_allowed_response("GET", "/api/nope").is_none());

        // Specific route wins over the {id} pattern.
        let resp = method_not_allowed_response("DELETE", "/api/feeds/refresh").unwrap();
        assert!(resp
            .headers
            .iter()
            .any(|(k, v)| k == "Allow" && v == "POST"));

        // Allowed method on a route that also matches a wildcard pattern.
        assert!(method_not_allowed_response("POST", "/api/feeds/refresh").is_none());
        assert!(method_not_allowed_response("GET", "/api/feeds/abc").is_some());
        assert!(method_not_allowed_response("PUT", "/api/feeds/refresh").is_some());

        // Query strings don't affect matching.
        let resp = method_not_allowed_response("POST", "/api/bangumi/search?name=x").unwrap();
        assert!(resp.headers.iter().any(|(k, v)| k == "Allow" && v == "GET"));
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
    fn test_is_valid_rss_url() {
        assert!(is_valid_rss_url("https://example.com/feed.xml"));
        assert!(is_valid_rss_url("http://example.com/feed"));
        assert!(!is_valid_rss_url(""));
        assert!(!is_valid_rss_url("ftp://example.com"));
        assert!(!is_valid_rss_url("not-a-url"));
    }
}
