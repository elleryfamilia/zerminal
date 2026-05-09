//! HTTP/1.1 request reader for the Copilot `/ide` Unix-socket transport.
//!
//! Caller invariants:
//! - Pass an `AsyncRead + Unpin` (e.g. `smol::net::unix::UnixStream`).
//! - Reuse one `RequestReader` per connection so leftover bytes from a
//!   pipelined or keep-alive next request stay buffered.
//! - On `Ok(None)` the peer cleanly closed the connection — drop the
//!   reader.
//! - On `Err(e)`, call `e.status_code()` to decide whether to send an
//!   HTTP error response (for protocol violations) or just close the
//!   connection (for I/O / EOF errors).
//!
//! The reader enforces hard limits to keep one slow or hostile peer from
//! consuming arbitrary memory:
//! - Headers section ≤ 8 KiB (else 431).
//! - Body ≤ 10 MiB (matches Copilot's `app.use(express.json({limit:'10mb'}))`).
//! - Chunked size lines ≤ 256 bytes; trailers ≤ 1 KiB total.

use futures::AsyncRead;
use futures::io::AsyncReadExt;
use http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode};

const HEADER_BUFFER_CAP: usize = 8 * 1024;
const BODY_LIMIT: usize = 10 * 1024 * 1024;
const CHUNK_SIZE_LINE_CAP: usize = 256;
const TRAILER_CAP: usize = 1024;
const MAX_HEADERS: usize = 64;
const READ_CHUNK_SIZE: usize = 4096;

#[derive(Debug)]
pub enum ReadError {
    Io(std::io::Error),
    HeaderTooLarge,
    HeaderParse(String),
    InvalidRequestLine,
    UnsupportedHttpVersion,
    InvalidPath,
    DuplicateContentLength,
    ConflictingFraming,
    LengthRequired,
    BodyTooLarge,
    BadChunked,
    UnsupportedTransferEncoding,
    UnknownExpectation,
    EofMidBody,
}

impl ReadError {
    /// HTTP status code to send on this error, or `None` if the connection
    /// should just be closed (I/O failures, mid-body EOF — no response can be
    /// safely sent).
    pub fn status_code(&self) -> Option<StatusCode> {
        match self {
            ReadError::Io(_) | ReadError::EofMidBody => None,
            ReadError::HeaderTooLarge => Some(StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE),
            ReadError::HeaderParse(_) => Some(StatusCode::BAD_REQUEST),
            ReadError::InvalidRequestLine => Some(StatusCode::BAD_REQUEST),
            ReadError::UnsupportedHttpVersion => Some(StatusCode::HTTP_VERSION_NOT_SUPPORTED),
            ReadError::InvalidPath => Some(StatusCode::BAD_REQUEST),
            ReadError::DuplicateContentLength => Some(StatusCode::BAD_REQUEST),
            ReadError::ConflictingFraming => Some(StatusCode::BAD_REQUEST),
            ReadError::LengthRequired => Some(StatusCode::LENGTH_REQUIRED),
            ReadError::BodyTooLarge => Some(StatusCode::PAYLOAD_TOO_LARGE),
            ReadError::BadChunked => Some(StatusCode::BAD_REQUEST),
            ReadError::UnsupportedTransferEncoding => {
                // RFC: "Server that receives a request message with a
                // transfer coding it does not understand SHOULD respond with
                // 501 Not Implemented".
                Some(StatusCode::NOT_IMPLEMENTED)
            }
            ReadError::UnknownExpectation => Some(StatusCode::EXPECTATION_FAILED),
        }
    }
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadError::Io(e) => write!(f, "I/O error: {e}"),
            ReadError::HeaderTooLarge => write!(f, "request headers exceed {HEADER_BUFFER_CAP} bytes"),
            ReadError::HeaderParse(s) => write!(f, "header parse error: {s}"),
            ReadError::InvalidRequestLine => write!(f, "invalid HTTP request line"),
            ReadError::UnsupportedHttpVersion => write!(f, "only HTTP/1.1 is supported"),
            ReadError::InvalidPath => write!(f, "request target must be origin-form"),
            ReadError::DuplicateContentLength => write!(f, "duplicate / conflicting Content-Length"),
            ReadError::ConflictingFraming => {
                write!(f, "Content-Length and Transfer-Encoding both present (request smuggling vector)")
            }
            ReadError::LengthRequired => write!(f, "Content-Length or Transfer-Encoding required for body"),
            ReadError::BodyTooLarge => write!(f, "body exceeds {BODY_LIMIT} bytes"),
            ReadError::BadChunked => write!(f, "malformed chunked encoding"),
            ReadError::UnsupportedTransferEncoding => write!(f, "unsupported Transfer-Encoding"),
            ReadError::UnknownExpectation => write!(f, "unknown Expect value"),
            ReadError::EofMidBody => write!(f, "EOF before body framing complete"),
        }
    }
}

impl std::error::Error for ReadError {}

#[derive(Debug)]
pub struct RequestParts {
    pub method: Method,
    /// Origin-form request target as sent by the client (e.g. `/mcp`).
    pub path: String,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

pub struct RequestReader<R> {
    stream: R,
    /// Bytes read from the wire that haven't yet been consumed by a parsed
    /// request. Carries leftover into the next call so pipelined or keep-alive
    /// peers don't lose framing.
    buffer: Vec<u8>,
}

impl<R: AsyncRead + Unpin> RequestReader<R> {
    pub fn new(stream: R) -> Self {
        Self {
            stream,
            buffer: Vec::with_capacity(READ_CHUNK_SIZE),
        }
    }

    /// Read one request from the wire. Returns `Ok(None)` on a clean EOF
    /// before any bytes (the peer closed without starting another request).
    pub async fn read_request(&mut self) -> Result<Option<RequestParts>, ReadError> {
        let header_end_idx = match self.read_headers().await? {
            Some(idx) => idx,
            None => return Ok(None),
        };

        // Re-parse to extract values now that we know the headers are
        // complete. Cheap; httparse is a streaming state machine.
        let mut headers_buf = [httparse::EMPTY_HEADER; MAX_HEADERS];
        let mut req = httparse::Request::new(&mut headers_buf);
        match req.parse(&self.buffer) {
            Ok(httparse::Status::Complete(_)) => {}
            _ => return Err(ReadError::InvalidRequestLine),
        }

        let method_str = req.method.ok_or(ReadError::InvalidRequestLine)?;
        let method = Method::from_bytes(method_str.as_bytes())
            .map_err(|_| ReadError::InvalidRequestLine)?;
        // httparse: 0 = HTTP/1.0, 1 = HTTP/1.1.
        let version = req.version.ok_or(ReadError::InvalidRequestLine)?;
        if version != 1 {
            return Err(ReadError::UnsupportedHttpVersion);
        }
        let path = req.path.ok_or(ReadError::InvalidRequestLine)?.to_string();
        // Origin-form only — reject absolute-form, asterisk-form, and authority-form.
        if !path.starts_with('/') {
            return Err(ReadError::InvalidPath);
        }

        let mut header_map = HeaderMap::new();
        let mut content_length: Option<usize> = None;
        let mut transfer_encoding: Option<String> = None;
        let mut expect_header: Option<String> = None;

        for h in req.headers.iter() {
            let name = HeaderName::from_bytes(h.name.as_bytes())
                .map_err(|_| ReadError::HeaderParse(format!("invalid header name: {}", h.name)))?;
            let value = HeaderValue::from_bytes(h.value)
                .map_err(|_| ReadError::HeaderParse(format!("invalid header value for {}", h.name)))?;

            if name == http::header::CONTENT_LENGTH {
                let s = std::str::from_utf8(h.value)
                    .map_err(|_| ReadError::HeaderParse("non-utf8 Content-Length".into()))?;
                let n: usize = s
                    .trim()
                    .parse()
                    .map_err(|_| ReadError::HeaderParse(format!("bad Content-Length: {s}")))?;
                if let Some(prev) = content_length {
                    if prev != n {
                        return Err(ReadError::DuplicateContentLength);
                    }
                }
                content_length = Some(n);
            } else if name == http::header::TRANSFER_ENCODING {
                let s = std::str::from_utf8(h.value)
                    .map_err(|_| ReadError::HeaderParse("non-utf8 Transfer-Encoding".into()))?;
                // RFC 7230: multiple TE values combine; we just lowercase and check.
                if let Some(existing) = transfer_encoding.as_mut() {
                    existing.push(',');
                    existing.push_str(s);
                } else {
                    transfer_encoding = Some(s.to_string());
                }
            } else if name == http::header::EXPECT {
                let s = std::str::from_utf8(h.value)
                    .map_err(|_| ReadError::HeaderParse("non-utf8 Expect".into()))?;
                expect_header = Some(s.to_ascii_lowercase());
            }

            header_map.append(name, value);
        }

        if content_length.is_some() && transfer_encoding.is_some() {
            // RFC 7230 §3.3.3: this is a request smuggling vector. Reject hard.
            return Err(ReadError::ConflictingFraming);
        }
        if let Some(expect) = expect_header.as_deref() {
            if expect.trim() != "100-continue" {
                return Err(ReadError::UnknownExpectation);
            }
            // 100-continue handling deferred to a later commit. v1 trusts the
            // peer to send the body anyway (matches curl, Copilot CLI
            // observed behavior).
        }

        // Drain headers from the buffer; what's left is the start of the body.
        self.buffer.drain(..header_end_idx);

        let body = if let Some(te) = transfer_encoding {
            if te
                .split(',')
                .map(|s| s.trim().to_ascii_lowercase())
                .any(|s| s == "chunked")
            {
                self.read_chunked_body().await?
            } else {
                return Err(ReadError::UnsupportedTransferEncoding);
            }
        } else if let Some(len) = content_length {
            if len > BODY_LIMIT {
                return Err(ReadError::BodyTooLarge);
            }
            self.read_exact_body(len).await?
        } else {
            // No body framing. Methods that semantically can have a body need
            // explicit framing per our policy.
            if matches!(method, Method::POST | Method::PUT | Method::PATCH) {
                return Err(ReadError::LengthRequired);
            }
            Vec::new()
        };

        Ok(Some(RequestParts {
            method,
            path,
            headers: header_map,
            body,
        }))
    }

    /// Returns `Some(idx)` where `buffer[..idx]` is the headers section
    /// (including the trailing `\r\n\r\n`), or `None` on clean EOF before any
    /// bytes were read.
    async fn read_headers(&mut self) -> Result<Option<usize>, ReadError> {
        loop {
            // Try to parse what we have so far.
            let mut headers_buf = [httparse::EMPTY_HEADER; MAX_HEADERS];
            let mut req = httparse::Request::new(&mut headers_buf);
            match req.parse(&self.buffer) {
                Ok(httparse::Status::Complete(n)) => return Ok(Some(n)),
                Ok(httparse::Status::Partial) => {
                    if self.buffer.len() >= HEADER_BUFFER_CAP {
                        return Err(ReadError::HeaderTooLarge);
                    }
                    if !self.read_more().await? {
                        if self.buffer.is_empty() {
                            return Ok(None);
                        }
                        return Err(ReadError::HeaderParse("EOF in headers".into()));
                    }
                }
                Err(e) => return Err(ReadError::HeaderParse(e.to_string())),
            }
        }
    }

    /// Pulls more bytes from the underlying stream into `self.buffer`. Returns
    /// `Ok(false)` on clean EOF.
    async fn read_more(&mut self) -> Result<bool, ReadError> {
        let mut tmp = [0u8; READ_CHUNK_SIZE];
        match self.stream.read(&mut tmp).await {
            Ok(0) => Ok(false),
            Ok(n) => {
                self.buffer.extend_from_slice(&tmp[..n]);
                Ok(true)
            }
            Err(e) => Err(ReadError::Io(e)),
        }
    }

    async fn read_exact_body(&mut self, len: usize) -> Result<Vec<u8>, ReadError> {
        while self.buffer.len() < len {
            if !self.read_more().await? {
                return Err(ReadError::EofMidBody);
            }
        }
        let body = self.buffer[..len].to_vec();
        self.buffer.drain(..len);
        Ok(body)
    }

    async fn read_chunked_body(&mut self) -> Result<Vec<u8>, ReadError> {
        let mut out = Vec::new();
        loop {
            // Find the size line.
            let crlf_idx = loop {
                if let Some(pos) = find_crlf(&self.buffer) {
                    break pos;
                }
                if self.buffer.len() > CHUNK_SIZE_LINE_CAP {
                    return Err(ReadError::BadChunked);
                }
                if !self.read_more().await? {
                    return Err(ReadError::BadChunked);
                }
            };
            if crlf_idx > CHUNK_SIZE_LINE_CAP {
                return Err(ReadError::BadChunked);
            }
            let line = std::str::from_utf8(&self.buffer[..crlf_idx])
                .map_err(|_| ReadError::BadChunked)?;
            let size_hex = line.split(';').next().unwrap_or("").trim();
            let size = usize::from_str_radix(size_hex, 16).map_err(|_| ReadError::BadChunked)?;
            self.buffer.drain(..crlf_idx + 2);

            if size == 0 {
                // Drain trailers (each ends in CRLF; final blank line ends the body).
                let mut trailer_total = 0usize;
                loop {
                    let crlf_idx = loop {
                        if let Some(pos) = find_crlf(&self.buffer) {
                            break pos;
                        }
                        if self.buffer.len() > TRAILER_CAP {
                            return Err(ReadError::BadChunked);
                        }
                        if !self.read_more().await? {
                            return Err(ReadError::BadChunked);
                        }
                    };
                    if crlf_idx == 0 {
                        // Blank line — end of trailers and end of body.
                        self.buffer.drain(..2);
                        break;
                    }
                    trailer_total += crlf_idx + 2;
                    if trailer_total > TRAILER_CAP {
                        return Err(ReadError::BadChunked);
                    }
                    self.buffer.drain(..crlf_idx + 2);
                }
                return Ok(out);
            }

            if out.len().saturating_add(size) > BODY_LIMIT {
                return Err(ReadError::BodyTooLarge);
            }
            while self.buffer.len() < size + 2 {
                if !self.read_more().await? {
                    return Err(ReadError::BadChunked);
                }
            }
            if &self.buffer[size..size + 2] != b"\r\n" {
                return Err(ReadError::BadChunked);
            }
            out.extend_from_slice(&self.buffer[..size]);
            self.buffer.drain(..size + 2);
        }
    }
}

fn find_crlf(buf: &[u8]) -> Option<usize> {
    buf.windows(2).position(|w| w == b"\r\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::io::Cursor;

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        smol::block_on(f)
    }

    fn reader_for(bytes: &[u8]) -> RequestReader<Cursor<Vec<u8>>> {
        RequestReader::new(Cursor::new(bytes.to_vec()))
    }

    #[test]
    fn reads_simple_post_with_content_length() {
        let raw = b"POST /mcp HTTP/1.1\r\n\
            Host: localhost\r\n\
            Content-Type: application/json\r\n\
            Content-Length: 13\r\n\
            \r\n\
            hello, world!";
        let mut r = reader_for(raw);
        let req = block_on(r.read_request()).expect("ok").expect("Some");
        assert_eq!(req.method, Method::POST);
        assert_eq!(req.path, "/mcp");
        assert_eq!(req.body, b"hello, world!");
        assert_eq!(
            req.headers.get(http::header::CONTENT_TYPE).unwrap(),
            "application/json"
        );
    }

    #[test]
    fn reads_chunked_body() {
        // Three chunks: "Hello", " ", "World!" → total "Hello World!"
        let raw = b"POST /mcp HTTP/1.1\r\n\
            Transfer-Encoding: chunked\r\n\
            \r\n\
            5\r\nHello\r\n\
            1\r\n \r\n\
            6\r\nWorld!\r\n\
            0\r\n\r\n";
        let mut r = reader_for(raw);
        let req = block_on(r.read_request()).expect("ok").expect("Some");
        assert_eq!(req.body, b"Hello World!");
    }

    #[test]
    fn reads_chunked_with_extension_and_trailer() {
        // Real Copilot traffic uses "a1\r\n...\r\n0\r\n\r\n" with no extension or
        // trailer. We additionally exercise the chunk extension and trailer
        // parsing branches here.
        let raw = b"POST /mcp HTTP/1.1\r\n\
            Transfer-Encoding: chunked\r\n\
            \r\n\
            3;ext=foo\r\nabc\r\n\
            0\r\n\
            X-Foo: bar\r\n\
            \r\n";
        let mut r = reader_for(raw);
        let req = block_on(r.read_request()).expect("ok").expect("Some");
        assert_eq!(req.body, b"abc");
    }

    #[test]
    fn rejects_content_length_plus_transfer_encoding() {
        let raw = b"POST /mcp HTTP/1.1\r\n\
            Content-Length: 5\r\n\
            Transfer-Encoding: chunked\r\n\
            \r\n\
            hello";
        let mut r = reader_for(raw);
        let err = block_on(r.read_request()).expect_err("must reject");
        assert!(matches!(err, ReadError::ConflictingFraming));
        assert_eq!(err.status_code(), Some(StatusCode::BAD_REQUEST));
    }

    #[test]
    fn rejects_duplicate_conflicting_content_length() {
        let raw = b"POST /mcp HTTP/1.1\r\n\
            Content-Length: 5\r\n\
            Content-Length: 13\r\n\
            \r\n\
            hello, world!";
        let mut r = reader_for(raw);
        let err = block_on(r.read_request()).expect_err("must reject");
        assert!(matches!(err, ReadError::DuplicateContentLength));
    }

    #[test]
    fn accepts_duplicate_matching_content_length() {
        // Two CL headers with the same value are not a conflict per RFC 7230.
        let raw = b"POST /mcp HTTP/1.1\r\n\
            Content-Length: 5\r\n\
            Content-Length: 5\r\n\
            \r\n\
            hello";
        let mut r = reader_for(raw);
        let req = block_on(r.read_request()).expect("ok").expect("Some");
        assert_eq!(req.body, b"hello");
    }

    #[test]
    fn rejects_http_1_0() {
        let raw = b"GET /mcp HTTP/1.0\r\n\r\n";
        let mut r = reader_for(raw);
        let err = block_on(r.read_request()).expect_err("must reject");
        assert!(matches!(err, ReadError::UnsupportedHttpVersion));
        assert_eq!(err.status_code(), Some(StatusCode::HTTP_VERSION_NOT_SUPPORTED));
    }

    #[test]
    fn rejects_absolute_form_request_target() {
        let raw = b"GET http://example.com/mcp HTTP/1.1\r\nHost: x\r\n\r\n";
        let mut r = reader_for(raw);
        let err = block_on(r.read_request()).expect_err("must reject");
        assert!(matches!(err, ReadError::InvalidPath));
    }

    #[test]
    fn rejects_post_without_body_framing() {
        let raw = b"POST /mcp HTTP/1.1\r\nHost: x\r\n\r\n";
        let mut r = reader_for(raw);
        let err = block_on(r.read_request()).expect_err("must reject");
        assert!(matches!(err, ReadError::LengthRequired));
        assert_eq!(err.status_code(), Some(StatusCode::LENGTH_REQUIRED));
    }

    #[test]
    fn allows_get_without_body() {
        let raw = b"GET /mcp HTTP/1.1\r\nHost: x\r\n\r\n";
        let mut r = reader_for(raw);
        let req = block_on(r.read_request()).expect("ok").expect("Some");
        assert_eq!(req.method, Method::GET);
        assert!(req.body.is_empty());
    }

    #[test]
    fn rejects_oversized_headers() {
        let mut raw = b"GET /mcp HTTP/1.1\r\n".to_vec();
        // Pad past the 8 KiB cap.
        for i in 0..200 {
            raw.extend_from_slice(format!("X-Pad-{i}: ").as_bytes());
            raw.extend_from_slice(&vec![b'a'; 200]);
            raw.extend_from_slice(b"\r\n");
        }
        raw.extend_from_slice(b"\r\n");
        let mut r = reader_for(&raw);
        let err = block_on(r.read_request()).expect_err("must reject");
        assert!(matches!(err, ReadError::HeaderTooLarge));
        assert_eq!(
            err.status_code(),
            Some(StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE)
        );
    }

    #[test]
    fn rejects_oversized_body() {
        let len = BODY_LIMIT + 1;
        let raw = format!(
            "POST /mcp HTTP/1.1\r\nContent-Length: {len}\r\n\r\n"
        )
        .into_bytes();
        let mut r = reader_for(&raw);
        let err = block_on(r.read_request()).expect_err("must reject");
        assert!(matches!(err, ReadError::BodyTooLarge));
        assert_eq!(err.status_code(), Some(StatusCode::PAYLOAD_TOO_LARGE));
    }

    #[test]
    fn returns_none_on_clean_eof_before_request() {
        let mut r = reader_for(b"");
        let result = block_on(r.read_request()).expect("ok");
        assert!(result.is_none());
    }

    #[test]
    fn errors_on_eof_mid_headers() {
        let mut r = reader_for(b"POST /mcp HTTP/1.1\r\nHost: ");
        let err = block_on(r.read_request()).expect_err("must error");
        assert!(matches!(err, ReadError::HeaderParse(_)));
    }

    #[test]
    fn errors_on_eof_mid_body() {
        // Says CL=10 but only sends 4 body bytes.
        let raw = b"POST /mcp HTTP/1.1\r\nContent-Length: 10\r\n\r\nhi!\n";
        let mut r = reader_for(raw);
        let err = block_on(r.read_request()).expect_err("must error");
        assert!(matches!(err, ReadError::EofMidBody));
        assert_eq!(err.status_code(), None, "EOF mid-body cannot send a response");
    }

    #[test]
    fn errors_on_malformed_chunked() {
        // "zz" is not a valid hex chunk size.
        let raw = b"POST /mcp HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n";
        let mut r = reader_for(raw);
        let err = block_on(r.read_request()).expect_err("must error");
        assert!(matches!(err, ReadError::BadChunked));
    }

    #[test]
    fn rejects_unknown_transfer_encoding() {
        let raw = b"POST /mcp HTTP/1.1\r\nTransfer-Encoding: gzip\r\n\r\nx";
        let mut r = reader_for(raw);
        let err = block_on(r.read_request()).expect_err("must error");
        assert!(matches!(err, ReadError::UnsupportedTransferEncoding));
        assert_eq!(err.status_code(), Some(StatusCode::NOT_IMPLEMENTED));
    }

    #[test]
    fn rejects_unknown_expectation() {
        let raw = b"POST /mcp HTTP/1.1\r\nExpect: 200-please\r\nContent-Length: 0\r\n\r\n";
        let mut r = reader_for(raw);
        let err = block_on(r.read_request()).expect_err("must error");
        assert!(matches!(err, ReadError::UnknownExpectation));
        assert_eq!(err.status_code(), Some(StatusCode::EXPECTATION_FAILED));
    }

    #[test]
    fn handles_keep_alive_two_requests_on_one_connection() {
        let raw = b"GET /mcp HTTP/1.1\r\nHost: x\r\n\r\n\
            POST /mcp HTTP/1.1\r\nContent-Length: 4\r\n\r\nping";
        let mut r = reader_for(raw);
        let first = block_on(r.read_request()).expect("ok").expect("Some");
        assert_eq!(first.method, Method::GET);
        let second = block_on(r.read_request()).expect("ok").expect("Some");
        assert_eq!(second.method, Method::POST);
        assert_eq!(second.body, b"ping");
        // Third call: clean EOF.
        let third = block_on(r.read_request()).expect("ok");
        assert!(third.is_none());
    }

    #[test]
    fn handles_chunked_split_body_already_buffered() {
        // Single read produces both headers and full chunked body in one shot.
        // Verifies that body bytes already buffered alongside headers (the bug
        // Codex flagged) are correctly preserved across the header/body boundary.
        let raw = b"POST /mcp HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nHello\r\n0\r\n\r\n";
        let mut r = reader_for(raw);
        let req = block_on(r.read_request()).expect("ok").expect("Some");
        assert_eq!(req.body, b"Hello");
    }

    #[test]
    fn parses_real_copilot_initialize_request() {
        // Captured from `@github/copilot` v1.0.44 via the recording_shim.
        // Validates the reader handles the actual wire format.
        let raw = b"POST /mcp HTTP/1.1\r\n\
            X-Copilot-Session-Id: 6095c4c2-958b-43a0-aaec-d194832ea3df\r\n\
            X-Copilot-PID: 62368\r\n\
            X-Copilot-Parent-PID: 62367\r\n\
            Authorization: Nonce ddb39de7-2787-4977-a521-6e0ea140ff30\r\n\
            accept: application/json, text/event-stream\r\n\
            content-type: application/json\r\n\
            Host: localhost\r\n\
            Connection: keep-alive\r\n\
            Transfer-Encoding: chunked\r\n\
            \r\n\
            a1\r\n\
            {\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-11-25\",\"capabilities\":{},\"clientInfo\":{\"name\":\"copilot-cli\",\"version\":\"1.0.44\"}},\"jsonrpc\":\"2.0\",\"id\":0}\r\n\
            0\r\n\r\n";
        let mut r = reader_for(raw);
        let req = block_on(r.read_request()).expect("ok").expect("Some");
        assert_eq!(req.method, Method::POST);
        assert_eq!(req.path, "/mcp");
        assert_eq!(
            req.headers
                .get("x-copilot-session-id")
                .and_then(|v| v.to_str().ok()),
            Some("6095c4c2-958b-43a0-aaec-d194832ea3df")
        );
        assert_eq!(
            req.headers
                .get(http::header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok()),
            Some("Nonce ddb39de7-2787-4977-a521-6e0ea140ff30")
        );
        let parsed: serde_json::Value = serde_json::from_slice(&req.body).expect("body parses as json");
        assert_eq!(parsed["method"], "initialize");
        assert_eq!(parsed["params"]["protocolVersion"], "2025-11-25");
    }
}
