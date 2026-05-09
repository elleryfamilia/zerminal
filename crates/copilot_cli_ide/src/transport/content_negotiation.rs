//! Accept and Content-Type header matching for MCP Streamable HTTP rules:
//!
//! - POST `/mcp`: `Content-Type: application/json` (charset param tolerated);
//!   `Accept` must include both `application/json` and `text/event-stream`.
//! - GET `/mcp`: `Accept` must include `text/event-stream`.
//!
//! `*/*` and `<type>/*` wildcards are honored. `q=0` parameters mark a media
//! type as not-accepted and exclude it from a match. Comma-separated entries
//! and repeated header values are both supported.
//!
//! Codex review v1 / v2 specifically called out that "Accept contains X" is
//! too loose — handle q-values, charset params, wildcards, and case
//! correctly.

use http::HeaderMap;

/// Returns true if the request's `Accept` header(s) accept `target` (e.g.
/// `"application/json"` or `"text/event-stream"`).
///
/// Per RFC 7231 §5.3.2, an absent `Accept` header means "accept anything",
/// so we treat that as a match.
pub fn accepts(headers: &HeaderMap, target: &str) -> bool {
    let (target_type, target_subtype) = match split_media_type(target) {
        Some(parts) => parts,
        None => return false,
    };

    let mut saw_any = false;
    for value in headers.get_all(http::header::ACCEPT).iter() {
        saw_any = true;
        let Ok(s) = value.to_str() else { continue };
        for entry in s.split(',') {
            if entry_matches(entry, target_type, target_subtype) {
                return true;
            }
        }
    }
    !saw_any
}

/// Returns true if the request's `Content-Type` header matches `target`,
/// ignoring parameters (`charset`, etc.). Multiple `Content-Type` headers
/// are not allowed by HTTP, but if one happens to be present we accept the
/// first.
pub fn content_type_is(headers: &HeaderMap, target: &str) -> bool {
    let (target_type, target_subtype) = match split_media_type(target) {
        Some(parts) => parts,
        None => return false,
    };
    let Some(value) = headers.get(http::header::CONTENT_TYPE) else {
        return false;
    };
    let Ok(s) = value.to_str() else { return false };
    let media_type = s.split(';').next().unwrap_or("").trim();
    let Some((ty, sub)) = split_media_type(media_type) else {
        return false;
    };
    ty.eq_ignore_ascii_case(target_type) && sub.eq_ignore_ascii_case(target_subtype)
}

fn entry_matches(entry: &str, target_type: &str, target_subtype: &str) -> bool {
    let mut parts = entry.split(';').map(str::trim);
    let media_type = parts.next().unwrap_or("");
    let Some((ty, sub)) = split_media_type(media_type) else {
        return false;
    };

    // Reject q=0 (explicitly not accepted).
    for param in parts {
        if let Some((name, value)) = param.split_once('=') {
            if name.trim().eq_ignore_ascii_case("q") {
                let v = value.trim().trim_matches('"');
                if let Ok(q) = v.parse::<f32>() {
                    if q <= 0.0 {
                        return false;
                    }
                }
            }
        }
    }

    let type_matches = ty == "*" || ty.eq_ignore_ascii_case(target_type);
    let subtype_matches = sub == "*" || sub.eq_ignore_ascii_case(target_subtype);
    type_matches && subtype_matches
}

fn split_media_type(s: &str) -> Option<(&str, &str)> {
    let s = s.trim();
    let (ty, sub) = s.split_once('/')?;
    let ty = ty.trim();
    let sub = sub.trim();
    if ty.is_empty() || sub.is_empty() {
        return None;
    }
    Some((ty, sub))
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::HeaderValue;

    fn headers_from(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (name, value) in pairs {
            h.append(
                http::HeaderName::from_bytes(name.as_bytes()).expect("valid header name"),
                HeaderValue::from_str(value).expect("valid header value"),
            );
        }
        h
    }

    #[test]
    fn accept_with_exact_match() {
        let h = headers_from(&[("Accept", "application/json")]);
        assert!(accepts(&h, "application/json"));
        assert!(!accepts(&h, "text/event-stream"));
    }

    #[test]
    fn accept_with_comma_separated_includes_both() {
        let h = headers_from(&[("Accept", "application/json, text/event-stream")]);
        assert!(accepts(&h, "application/json"));
        assert!(accepts(&h, "text/event-stream"));
    }

    #[test]
    fn accept_star_star_matches_anything() {
        let h = headers_from(&[("Accept", "*/*")]);
        assert!(accepts(&h, "application/json"));
        assert!(accepts(&h, "text/event-stream"));
        assert!(accepts(&h, "image/png"));
    }

    #[test]
    fn accept_subtype_wildcard() {
        let h = headers_from(&[("Accept", "application/*")]);
        assert!(accepts(&h, "application/json"));
        assert!(accepts(&h, "application/xml"));
        assert!(!accepts(&h, "text/event-stream"));
    }

    #[test]
    fn accept_q_zero_excludes() {
        let h = headers_from(&[("Accept", "application/json;q=0, text/event-stream")]);
        assert!(!accepts(&h, "application/json"));
        assert!(accepts(&h, "text/event-stream"));
    }

    #[test]
    fn accept_q_value_with_other_params_still_matches() {
        // q=1 is the default; charset is a non-q param and shouldn't break matching.
        let h = headers_from(&[("Accept", "application/json; charset=utf-8; q=1")]);
        assert!(accepts(&h, "application/json"));
    }

    #[test]
    fn accept_is_case_insensitive() {
        let h = headers_from(&[("Accept", "APPLICATION/JSON")]);
        assert!(accepts(&h, "application/json"));
    }

    #[test]
    fn missing_accept_header_means_accept_anything() {
        let h = HeaderMap::new();
        assert!(accepts(&h, "application/json"));
        assert!(accepts(&h, "text/event-stream"));
    }

    #[test]
    fn accept_repeated_headers_are_combined() {
        let h = headers_from(&[
            ("Accept", "application/json"),
            ("Accept", "text/event-stream"),
        ]);
        assert!(accepts(&h, "application/json"));
        assert!(accepts(&h, "text/event-stream"));
    }

    #[test]
    fn real_copilot_accept_header_passes_both() {
        // Captured: `accept: application/json, text/event-stream`
        let h = headers_from(&[("accept", "application/json, text/event-stream")]);
        assert!(accepts(&h, "application/json"));
        assert!(accepts(&h, "text/event-stream"));
    }

    #[test]
    fn content_type_strict_match() {
        let h = headers_from(&[("Content-Type", "application/json")]);
        assert!(content_type_is(&h, "application/json"));
        assert!(!content_type_is(&h, "text/plain"));
    }

    #[test]
    fn content_type_with_charset_param() {
        let h = headers_from(&[("Content-Type", "application/json; charset=utf-8")]);
        assert!(content_type_is(&h, "application/json"));
    }

    #[test]
    fn content_type_case_insensitive() {
        let h = headers_from(&[("Content-Type", "Application/JSON")]);
        assert!(content_type_is(&h, "application/json"));
    }

    #[test]
    fn content_type_missing_returns_false() {
        let h = HeaderMap::new();
        assert!(!content_type_is(&h, "application/json"));
    }

    #[test]
    fn malformed_media_type_is_rejected() {
        let h = headers_from(&[("Content-Type", "garbage-no-slash")]);
        assert!(!content_type_is(&h, "application/json"));
    }

    #[test]
    fn whitespace_in_entries_is_tolerated() {
        let h = headers_from(&[("Accept", "  application/json  ,  text/event-stream  ")]);
        assert!(accepts(&h, "application/json"));
        assert!(accepts(&h, "text/event-stream"));
    }
}
