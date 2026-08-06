//! HTTP Range-request serving — pure data → HTTP Response.
//!
//! Entry point: [`resolve_range`] parses the `Range` header,
//! [`serve_file_range`] streams the requested byte range.
//! Both are pure functions with no side effects.

use rouille::{Request, Response, ResponseBody};

use crate::services::server::utils::problem_response;
use crate::types::FileStream;
use crate::types::{http_code, problem_type};

/// Parse the `Range` header from a `Request` and return the byte range to serve.
///
/// Returns:
/// - `Ok(None)` — no `Range` header, serve full file.
/// - `Ok(Some((start, end)))` — valid byte range (inclusive).
/// - `Err(Response)` — 416 Range Not Satisfiable (caller returns this directly).
pub fn resolve_range(request: &Request, file_size: u64) -> Result<Option<(u64, u64)>, Response> {
    let range_header = match request.header("Range") {
        Some(h) => h,
        None => return Ok(None),
    };
    let range_val = match range_header.strip_prefix("bytes=") {
        Some(v) => v.trim(),
        None => return Err(build_416(file_size)),
    };
    if range_val.contains(',') {
        return Err(build_416(file_size));
    }
    match parse_range(range_val, file_size) {
        Some(r) => Ok(Some(r)),
        None => Err(build_416(file_size)),
    }
}

/// Construct an HTTP response for a file byte range.
///
/// - `range = None` → 200 full file
/// - `range = Some((start, end))` → 206 partial content
pub fn serve_file_range(
    stream: FileStream,
    file_size: u64,
    content_type: &'static str,
    range: Option<(u64, u64)>,
) -> Response {
    // Empty file → always 200 with empty body.
    if file_size == 0 {
        return Response {
            status_code: http_code::OK,
            headers: vec![
                ("Content-Type".into(), content_type.into()),
                ("Accept-Ranges".into(), "bytes".into()),
            ],
            data: ResponseBody::empty(),
            upgrade: None,
        };
    }

    match range {
        None => {
            // No Range header → 200 full file.
            let reader = match stream.into_range(0, file_size) {
                Ok(r) => r,
                Err(_) => {
                    log::error!("seek failed: {file_size} bytes, {content_type}");
                    return problem_response(
                        http_code::INTERNAL,
                        problem_type::INTERNAL,
                        "Internal error",
                        "internal server error",
                    );
                }
            };
            log::info!("→ 200 OK ({content_type}, {file_size} bytes)");
            Response {
                status_code: http_code::OK,
                headers: vec![
                    ("Content-Type".into(), content_type.into()),
                    ("Accept-Ranges".into(), "bytes".into()),
                ],
                data: ResponseBody::from_reader_and_size(reader, file_size as usize),
                upgrade: None,
            }
        }
        Some((start, end)) => {
            // Valid Range → 206 partial content.
            let length = end - start + 1;
            let content_range = format!("bytes {start}-{end}/{file_size}");

            let reader = match stream.into_range(start, length) {
                Ok(r) => r,
                Err(_) => {
                    log::error!("seek failed: bytes {start}-{end}/{file_size}, {content_type}");
                    return problem_response(
                        http_code::INTERNAL,
                        problem_type::INTERNAL,
                        "Internal error",
                        "internal server error",
                    );
                }
            };

            log::info!("→ 206 bytes {start}-{end}/{file_size} ({content_type})");

            Response {
                status_code: http_code::PARTIAL_CONTENT,
                headers: vec![
                    ("Content-Type".into(), content_type.into()),
                    ("Content-Range".into(), content_range.into()),
                    ("Accept-Ranges".into(), "bytes".into()),
                ],
                data: ResponseBody::from_reader_and_size(reader, length as usize),
                upgrade: None,
            }
        }
    }
}

// ── Private helpers ──

/// 416 Range Not Satisfiable.
fn build_416(file_size: u64) -> Response {
    let mut resp = problem_response(
        http_code::RANGE_NOT_SATISFIABLE,
        problem_type::RANGE_NOT_SATISFIABLE,
        "Range Not Satisfiable",
        &format!("requested range is not satisfiable (file size {file_size})"),
    );
    resp.headers.push((
        "Content-Range".into(),
        format!("bytes */{file_size}").into(),
    ));
    resp
}

/// Parse a range string like `"0-499"` or `"-500"` into `(start, end)` inclusive.
/// Returns `None` if the range is invalid (out of bounds, unparseable, etc.).
fn parse_range(range: &str, file_size: u64) -> Option<(u64, u64)> {
    // Suffix range: "-N" → last N bytes.
    if let Some(suffix) = range.strip_prefix('-') {
        let n: u64 = suffix.parse().ok()?;
        if n == 0 {
            return None;
        }
        let start = file_size.saturating_sub(n);
        let end = file_size - 1;
        return Some((start, end));
    }

    // Standard range: "A-B".
    let (start_str, end_str) = range.split_once('-')?;
    let start: u64 = start_str.parse().ok()?;
    if start >= file_size {
        return None;
    }
    let end: u64 = if end_str.is_empty() {
        file_size - 1
    } else {
        let e: u64 = end_str.parse().ok()?;
        if e >= file_size || e < start {
            return None;
        }
        e
    };
    Some((start, end))
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    // ── parse_range ──

    #[test]
    fn parse_range_standard() {
        assert_eq!(parse_range("0-4", 100), Some((0, 4)));
        assert_eq!(parse_range("50-", 100), Some((50, 99)));
        assert_eq!(parse_range("0-99", 100), Some((0, 99)));
    }

    #[test]
    fn parse_range_suffix() {
        assert_eq!(parse_range("-10", 100), Some((90, 99)));
        assert_eq!(parse_range("-1", 100), Some((99, 99)));
    }

    #[test]
    fn parse_range_invalid() {
        assert_eq!(parse_range("100-", 100), None);
        assert_eq!(parse_range("0-100", 100), None);
        assert_eq!(parse_range("-0", 100), None);
        assert_eq!(parse_range("abc", 100), None);
        assert_eq!(parse_range("", 100), None);
    }

    // ── resolve_range ──

    fn range_request(range: Option<&str>) -> Request {
        let headers = range
            .map(|h| vec![("Range".into(), h.into())])
            .unwrap_or_default();
        Request::fake_http("GET", "/file", headers, vec![])
    }

    #[test]
    fn resolve_no_range_header() {
        let req = range_request(None);
        let result = resolve_range(&req, 100);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), None);
    }

    #[test]
    fn resolve_valid_range() {
        let req = range_request(Some("bytes=0-9"));
        let result = resolve_range(&req, 100);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Some((0, 9)));
    }

    #[test]
    fn resolve_invalid_prefix_returns_416() {
        let req = range_request(Some("invalid=0-9"));
        let result = resolve_range(&req, 100);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().status_code, 416);
    }

    #[test]
    fn resolve_out_of_bounds_returns_416() {
        let req = range_request(Some("bytes=50-200"));
        let result = resolve_range(&req, 100);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().status_code, 416);
    }

    // ── serve_file_range ──

    fn read_body(resp: Response) -> Vec<u8> {
        let (mut reader, _) = resp.data.into_reader_and_size();
        let mut buf = Vec::new();
        reader.read_to_end(&mut buf).unwrap();
        buf
    }

    #[test]
    fn serve_full_file_no_range() {
        let data = b"hello world".to_vec();
        let stream = FileStream::new(std::io::Cursor::new(data), 11);
        let resp = serve_file_range(stream, 11, "text/plain", None);
        assert_eq!(resp.status_code, 200);
        assert!(resp.headers.iter().any(|(k, _)| k == "Accept-Ranges"));
    }

    #[test]
    fn serve_range_206() {
        let data = b"hello world".to_vec();
        let stream = FileStream::new(std::io::Cursor::new(data), 11);
        let resp = serve_file_range(stream, 11, "text/plain", Some((0, 4)));
        assert_eq!(resp.status_code, 206);
        assert!(resp
            .headers
            .iter()
            .any(|(k, v)| k == "Content-Range" && v == "bytes 0-4/11"));
    }

    #[test]
    fn serve_range_data_integrity() {
        let content = b"0123456789ABCDEF";
        let stream = FileStream::new(std::io::Cursor::new(content.to_vec()), 16);
        let resp = serve_file_range(stream, 16, "text/plain", Some((0, 3)));
        assert_eq!(resp.status_code, 206);
        let body = read_body(resp);
        assert_eq!(body, b"0123");
    }

    #[test]
    fn serve_empty_file() {
        let stream = FileStream::new(std::io::Cursor::new(vec![]), 0);
        let resp = serve_file_range(stream, 0, "text/plain", None);
        assert_eq!(resp.status_code, 200);
    }
}
