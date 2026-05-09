//! HTTP/1.1 response serialization. One-shot, fixed-Content-Length responses
//! only — the SSE stream writer is its own module.

use http::{HeaderMap, HeaderName, HeaderValue, StatusCode};

/// Build an HTTP/1.1 response wire bytes from a status, headers, and body.
/// `Content-Length` is always emitted. `Server` and `Connection: keep-alive`
/// are added if the caller didn't supply them.
pub fn serialize_response(status: StatusCode, headers: &HeaderMap, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(256 + body.len());
    let reason = status.canonical_reason().unwrap_or("");
    out.extend_from_slice(b"HTTP/1.1 ");
    out.extend_from_slice(status.as_str().as_bytes());
    out.push(b' ');
    out.extend_from_slice(reason.as_bytes());
    out.extend_from_slice(b"\r\n");

    // Caller-supplied headers, except those we always set ourselves.
    for (name, value) in headers.iter() {
        if name == http::header::CONTENT_LENGTH || name == http::header::SERVER {
            // Skip — we set these ourselves to keep the bytes consistent.
            continue;
        }
        out.extend_from_slice(name.as_str().as_bytes());
        out.extend_from_slice(b": ");
        out.extend_from_slice(value.as_bytes());
        out.extend_from_slice(b"\r\n");
    }

    if !headers.contains_key(http::header::SERVER) {
        out.extend_from_slice(b"Server: Zerminal-Copilot-IDE/0.0.1\r\n");
    }
    if !headers.contains_key(http::header::CONNECTION) {
        out.extend_from_slice(b"Connection: keep-alive\r\n");
    }

    out.extend_from_slice(b"Content-Length: ");
    out.extend_from_slice(body.len().to_string().as_bytes());
    out.extend_from_slice(b"\r\n\r\n");
    out.extend_from_slice(body);
    out
}

/// Convenience: a `text/plain; charset=utf-8` response with a short reason
/// body. Used for error responses (401, 404, 411, etc.) where the body is
/// just informational.
pub fn plain_response(status: StatusCode, body: &str) -> Vec<u8> {
    let mut headers = HeaderMap::new();
    headers.insert(
        http::header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    serialize_response(status, &headers, body.as_bytes())
}

/// Convenience: a `200 OK` `application/json` response. Caller may pass extra
/// headers (e.g. `mcp-session-id`); they're appended.
#[allow(dead_code)]
pub fn json_response(status: StatusCode, body: &[u8], extra_headers: &[(HeaderName, HeaderValue)]) -> Vec<u8> {
    let mut headers = HeaderMap::new();
    headers.insert(
        http::header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    for (name, value) in extra_headers {
        headers.append(name.clone(), value.clone());
    }
    serialize_response(status, &headers, body)
}

/// Convenience: an empty-body response. Used for `202 Accepted` (notification
/// POSTs), `200 OK` DELETE responses, etc.
pub fn empty_response(status: StatusCode) -> Vec<u8> {
    let headers = HeaderMap::new();
    serialize_response(status, &headers, b"")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_response(bytes: &[u8]) -> (u16, Vec<(String, String)>, Vec<u8>) {
        let mut headers_buf = [httparse::EMPTY_HEADER; 32];
        let mut resp = httparse::Response::new(&mut headers_buf);
        let body_offset = resp.parse(bytes).expect("parse").unwrap();
        let code = resp.code.expect("code");
        let headers: Vec<(String, String)> = resp
            .headers
            .iter()
            .map(|h| {
                (
                    h.name.to_string(),
                    String::from_utf8_lossy(h.value).into_owned(),
                )
            })
            .collect();
        let body = bytes[body_offset..].to_vec();
        (code, headers, body)
    }

    #[test]
    fn empty_response_has_zero_content_length_and_keep_alive() {
        let bytes = empty_response(StatusCode::ACCEPTED);
        let (code, headers, body) = parse_response(&bytes);
        assert_eq!(code, 202);
        assert!(body.is_empty());
        assert_eq!(
            headers
                .iter()
                .find(|(n, _)| n.eq_ignore_ascii_case("content-length"))
                .map(|(_, v)| v.as_str()),
            Some("0")
        );
        assert!(
            headers
                .iter()
                .any(|(n, v)| n.eq_ignore_ascii_case("connection")
                    && v.eq_ignore_ascii_case("keep-alive"))
        );
        assert!(
            headers
                .iter()
                .any(|(n, _)| n.eq_ignore_ascii_case("server"))
        );
    }

    #[test]
    fn plain_response_carries_text_body() {
        let bytes = plain_response(StatusCode::UNAUTHORIZED, "no auth for you");
        let (code, headers, body) = parse_response(&bytes);
        assert_eq!(code, 401);
        assert_eq!(body, b"no auth for you");
        assert!(
            headers
                .iter()
                .any(|(n, v)| n.eq_ignore_ascii_case("content-type")
                    && v.starts_with("text/plain"))
        );
    }

    #[test]
    fn json_response_appends_extra_headers() {
        let extra = [(
            HeaderName::from_static("x-copilot-session-id"),
            HeaderValue::from_static("abc-123"),
        )];
        let bytes = json_response(StatusCode::OK, br#"{"ok":true}"#, &extra);
        let (code, headers, body) = parse_response(&bytes);
        assert_eq!(code, 200);
        assert_eq!(body, br#"{"ok":true}"#);
        assert!(
            headers
                .iter()
                .any(|(n, v)| n.eq_ignore_ascii_case("x-copilot-session-id") && v == "abc-123")
        );
        assert!(
            headers
                .iter()
                .any(|(n, v)| n.eq_ignore_ascii_case("content-type")
                    && v.eq_ignore_ascii_case("application/json"))
        );
    }

    #[test]
    fn caller_can_override_connection_header() {
        let mut h = HeaderMap::new();
        h.insert(http::header::CONNECTION, HeaderValue::from_static("close"));
        let bytes = serialize_response(StatusCode::OK, &h, b"x");
        let (_, headers, _) = parse_response(&bytes);
        let connection_values: Vec<&str> = headers
            .iter()
            .filter(|(n, _)| n.eq_ignore_ascii_case("connection"))
            .map(|(_, v)| v.as_str())
            .collect();
        assert_eq!(connection_values, vec!["close"]);
    }

    #[test]
    fn content_length_is_recomputed_even_if_caller_passes_one() {
        // Caller passing a wrong Content-Length must not corrupt our framing.
        let mut h = HeaderMap::new();
        h.insert(http::header::CONTENT_LENGTH, HeaderValue::from_static("999"));
        let bytes = serialize_response(StatusCode::OK, &h, b"hi");
        let (_, headers, body) = parse_response(&bytes);
        assert_eq!(body, b"hi");
        let cls: Vec<&str> = headers
            .iter()
            .filter(|(n, _)| n.eq_ignore_ascii_case("content-length"))
            .map(|(_, v)| v.as_str())
            .collect();
        assert_eq!(cls, vec!["2"]);
    }

    #[test]
    fn status_codes_use_canonical_reason() {
        let bytes = empty_response(StatusCode::NOT_FOUND);
        let line = std::str::from_utf8(&bytes[..bytes.iter().position(|&b| b == b'\r').unwrap()])
            .unwrap();
        assert_eq!(line, "HTTP/1.1 404 Not Found");
    }
}
