//! HTTP/1.1 server bound to a Unix domain socket. Glues the request reader,
//! response writer, content negotiation, and session store together. POST
//! handling is plugged in via the `PostHandler` trait so the MCP message
//! layer can be implemented and tested separately.
//!
//! Architecture (per Codex's note about Claude's pattern): socket I/O lives
//! on the background executor; the `PostHandler` is responsible for any
//! foreground hop it needs (e.g. dispatching to GPUI-bound editor state).

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use futures::AsyncWriteExt;
use futures::future::BoxFuture;
use http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use parking_lot::Mutex;
use smol::Task;
use smol::net::unix::UnixListener;

use crate::transport::content_negotiation::{accepts, content_type_is};
use crate::transport::reader::{RequestParts, RequestReader};
use crate::transport::session::{AttachError, SessionStore};
use crate::transport::writer::{empty_response, plain_response, serialize_response};

const SESSION_HEADER_PRIMARY: &str = "x-copilot-session-id";
const SESSION_HEADER_FALLBACK: &str = "mcp-session-id";

/// What a `PostHandler` returns. The server wraps this in proper HTTP
/// response framing.
pub struct PostResponse {
    pub status: StatusCode,
    pub body: Vec<u8>,
    pub extra_headers: Vec<(HeaderName, HeaderValue)>,
}

impl PostResponse {
    pub fn json(status: StatusCode, body: Vec<u8>) -> Self {
        Self {
            status,
            body,
            extra_headers: Vec::new(),
        }
    }

    pub fn accepted() -> Self {
        Self {
            status: StatusCode::ACCEPTED,
            body: Vec::new(),
            extra_headers: Vec::new(),
        }
    }

    pub fn with_header(mut self, name: HeaderName, value: HeaderValue) -> Self {
        self.extra_headers.push((name, value));
        self
    }
}

/// The MCP message layer plugs into the server here. The server has already
/// auth-checked the request and verified Accept / Content-Type before calling.
///
/// Returns a `BoxFuture` rather than `async fn` because we need
/// `Arc<dyn PostHandler>` and trait-object compatibility. The implementation
/// typically forwards to a foreground GPUI task for the EditorCapabilities
/// hop and awaits the result.
pub trait PostHandler: Send + Sync + 'static {
    fn handle_post(self: Arc<Self>, parts: RequestParts) -> BoxFuture<'static, PostResponse>;
}

pub struct Server {
    _accept_task: Task<()>,
    _connection_tasks: Arc<Mutex<Vec<Task<()>>>>,
}

impl Server {
    /// Bind a Unix domain socket at `socket_path` and start serving. Returns
    /// after the listener is live; per-connection work happens on
    /// background-spawned tasks. Drop the returned `Server` to stop accepting
    /// and cancel all in-flight connections.
    ///
    /// `nonce` is the auth-token value the lockfile advertises in
    /// `headers.Authorization` (the literal string after `Nonce `).
    pub fn bind(
        socket_path: PathBuf,
        nonce: String,
        session_store: SessionStore,
        post_handler: Arc<dyn PostHandler>,
    ) -> Result<Self> {
        let listener = UnixListener::bind(&socket_path)
            .with_context(|| format!("binding Unix socket {}", socket_path.display()))?;
        let connection_tasks = Arc::new(Mutex::new(Vec::<Task<()>>::new()));
        let accept_task = smol::spawn(run_accept_loop(
            listener,
            nonce,
            session_store,
            post_handler,
            connection_tasks.clone(),
        ));
        Ok(Self {
            _accept_task: accept_task,
            _connection_tasks: connection_tasks,
        })
    }
}

async fn run_accept_loop(
    listener: UnixListener,
    nonce: String,
    session_store: SessionStore,
    post_handler: Arc<dyn PostHandler>,
    connection_tasks: Arc<Mutex<Vec<Task<()>>>>,
) {
    log::info!(
        "Copilot /ide accept loop ready on {}",
        listener
            .local_addr()
            .ok()
            .and_then(|a| a.as_pathname().map(|p| p.display().to_string()))
            .unwrap_or_else(|| "<unknown>".to_string())
    );
    loop {
        let (stream, _addr) = match listener.accept().await {
            Ok(connection) => connection,
            Err(error) => {
                log::warn!("Copilot /ide accept failed: {error}");
                continue;
            }
        };
        log::debug!("Copilot /ide accept: new connection");
        let nonce = nonce.clone();
        let session_store = session_store.clone();
        let post_handler = post_handler.clone();
        let task = smol::spawn(async move {
            if let Err(e) =
                handle_connection(stream, nonce, session_store, post_handler).await
            {
                log::debug!("Copilot /ide connection ended with error: {e}");
            }
        });
        connection_tasks.lock().push(task);
    }
}

async fn handle_connection(
    stream: smol::net::unix::UnixStream,
    nonce: String,
    session_store: SessionStore,
    post_handler: Arc<dyn PostHandler>,
) -> Result<()> {
    let (read_half, write_half) = futures::io::AsyncReadExt::split(stream);
    let mut reader = RequestReader::new(read_half);
    let mut write_half = write_half;

    loop {
        let request = match reader.read_request().await {
            Ok(Some(req)) => req,
            Ok(None) => return Ok(()), // clean EOF
            Err(e) => {
                if let Some(status) = e.status_code() {
                    let response = plain_response(status, &e.to_string());
                    let _ = write_half.write_all(&response).await;
                }
                return Ok(());
            }
        };

        // Auth gate first — reject unauthorized callers before any further work.
        if !auth_ok(&request, &nonce) {
            let response = plain_response(StatusCode::UNAUTHORIZED, "unauthorized");
            write_half.write_all(&response).await?;
            // 401 doesn't preclude keep-alive, but in practice a misconfigured
            // client will not recover. Drop the connection to keep state clean.
            return Ok(());
        }

        let action = route(&request);
        log::debug!(
            "Copilot /ide request: method={} path={} body={}b",
            request.method,
            request.path,
            request.body.len()
        );
        match action {
            Route::Post => {
                if !content_type_is(&request.headers, "application/json") {
                    let response = plain_response(
                        StatusCode::UNSUPPORTED_MEDIA_TYPE,
                        "Content-Type must be application/json",
                    );
                    write_half.write_all(&response).await?;
                    continue;
                }
                if !accepts(&request.headers, "application/json")
                    || !accepts(&request.headers, "text/event-stream")
                {
                    let response = plain_response(
                        StatusCode::NOT_ACCEPTABLE,
                        "Accept must include application/json and text/event-stream",
                    );
                    write_half.write_all(&response).await?;
                    continue;
                }
                let result = post_handler.clone().handle_post(request).await;
                let mut headers = HeaderMap::new();
                if !result.body.is_empty() {
                    headers.insert(
                        http::header::CONTENT_TYPE,
                        HeaderValue::from_static("application/json"),
                    );
                }
                for (name, value) in &result.extra_headers {
                    headers.append(name.clone(), value.clone());
                }
                let response = serialize_response(result.status, &headers, &result.body);
                write_half.write_all(&response).await?;
            }
            Route::Get => {
                if !accepts(&request.headers, "text/event-stream") {
                    let response = plain_response(
                        StatusCode::NOT_ACCEPTABLE,
                        "Accept must include text/event-stream",
                    );
                    write_half.write_all(&response).await?;
                    continue;
                }
                let session_id = match extract_session_id(&request) {
                    Some(id) => id,
                    None => {
                        let response = plain_response(
                            StatusCode::BAD_REQUEST,
                            "session id required",
                        );
                        write_half.write_all(&response).await?;
                        continue;
                    }
                };
                log::info!("Copilot /ide GET /mcp: attaching SSE for session={session_id}");
                let receiver = match session_store.try_attach_sse(&session_id) {
                    Ok(r) => r,
                    Err(AttachError::NotFound) => {
                        let response = plain_response(
                            StatusCode::NOT_FOUND,
                            "session not found",
                        );
                        write_half.write_all(&response).await?;
                        continue;
                    }
                    Err(AttachError::AlreadyAttached) => {
                        let response = plain_response(
                            StatusCode::CONFLICT,
                            "stream already attached",
                        );
                        write_half.write_all(&response).await?;
                        continue;
                    }
                };
                // Send chunked-transfer SSE preamble. We keep this connection
                // dedicated to the SSE stream — once the receiver's loop ends
                // we close the connection (no keep-alive after SSE).
                write_half.write_all(SSE_PREAMBLE).await?;
                while let Ok(frame) = receiver.recv().await {
                    let chunk = format_sse_chunk(&frame);
                    if write_half.write_all(&chunk).await.is_err() {
                        break;
                    }
                }
                // Final zero-length chunk to terminate the chunked body.
                let _ = write_half.write_all(b"0\r\n\r\n").await;
                session_store.detach_sse(&session_id);
                return Ok(());
            }
            Route::Delete => {
                let session_id = match extract_session_id(&request) {
                    Some(id) => id,
                    None => {
                        let response = plain_response(
                            StatusCode::BAD_REQUEST,
                            "session id required",
                        );
                        write_half.write_all(&response).await?;
                        continue;
                    }
                };
                if session_store.delete(&session_id) {
                    let response = empty_response(StatusCode::OK);
                    write_half.write_all(&response).await?;
                } else {
                    let response =
                        plain_response(StatusCode::NOT_FOUND, "session not found");
                    write_half.write_all(&response).await?;
                }
            }
            Route::MethodNotAllowed => {
                let mut headers = HeaderMap::new();
                headers.insert(http::header::ALLOW, HeaderValue::from_static("POST, GET, DELETE"));
                let response = serialize_response(
                    StatusCode::METHOD_NOT_ALLOWED,
                    &headers,
                    b"method not allowed",
                );
                write_half.write_all(&response).await?;
            }
            Route::NotFound => {
                let response = plain_response(StatusCode::NOT_FOUND, "not found");
                write_half.write_all(&response).await?;
            }
        }
    }
}

enum Route {
    Post,
    Get,
    Delete,
    MethodNotAllowed,
    NotFound,
}

fn route(parts: &RequestParts) -> Route {
    if parts.path != "/mcp" {
        return Route::NotFound;
    }
    match parts.method {
        Method::POST => Route::Post,
        Method::GET => Route::Get,
        Method::DELETE => Route::Delete,
        _ => Route::MethodNotAllowed,
    }
}

fn auth_ok(parts: &RequestParts, nonce: &str) -> bool {
    let header = match parts
        .headers
        .get(http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    {
        Some(v) => v,
        None => return false,
    };
    let expected = format!("Nonce {nonce}");
    constant_time_eq(header.as_bytes(), expected.as_bytes())
}

/// Constant-time byte equality. Avoids leaking nonce length / prefix via
/// timing on the auth path. Hand-rolled rather than pulling in `subtle`.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn extract_session_id(parts: &RequestParts) -> Option<String> {
    parts
        .headers
        .get(SESSION_HEADER_PRIMARY)
        .or_else(|| parts.headers.get(SESSION_HEADER_FALLBACK))
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
}

const SSE_PREAMBLE: &[u8] = b"HTTP/1.1 200 OK\r\n\
    Content-Type: text/event-stream\r\n\
    Cache-Control: no-cache\r\n\
    Connection: keep-alive\r\n\
    Transfer-Encoding: chunked\r\n\
    \r\n";

/// Format a single SSE frame as one HTTP/1.1 chunked-transfer chunk:
/// `<hex-size>\r\ndata: <json>\r\n\r\n\r\n`.
fn format_sse_chunk(frame: &str) -> Vec<u8> {
    let payload = format!("data: {frame}\r\n\r\n");
    let mut out = Vec::with_capacity(payload.len() + 16);
    out.extend_from_slice(format!("{:x}\r\n", payload.len()).as_bytes());
    out.extend_from_slice(payload.as_bytes());
    out.extend_from_slice(b"\r\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::AsyncReadExt;
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Minimal HTTP/1.1 client just for tests — sends a request and reads back
    /// until EOF or an estimated full response is in.
    async fn send_and_read(socket: &Path, request: &[u8]) -> Vec<u8> {
        let mut stream = smol::net::unix::UnixStream::connect(socket)
            .await
            .expect("connect");
        stream.write_all(request).await.expect("write");
        // No half-close API on smol's UnixStream that I know of; just read
        // until EOF or short timeout. The server returns bounded bytes.
        let mut out = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            match smol::future::or(
                async { stream.read(&mut buf).await.map_err(|e| e.to_string()) },
                async {
                    smol::Timer::after(std::time::Duration::from_millis(150)).await;
                    Err("timeout".to_string())
                },
            )
            .await
            {
                Ok(0) => break,
                Ok(n) => out.extend_from_slice(&buf[..n]),
                Err(_) => break,
            }
        }
        out
    }

    fn parse_status(bytes: &[u8]) -> u16 {
        let mut headers_buf = [httparse::EMPTY_HEADER; 32];
        let mut resp = httparse::Response::new(&mut headers_buf);
        resp.parse(bytes).expect("parse").unwrap();
        resp.code.expect("code")
    }

    /// Test PostHandler that calls a stored closure.
    struct MockHandler {
        calls: AtomicUsize,
        response: PostResponse,
    }

    impl MockHandler {
        fn new(response: PostResponse) -> Arc<Self> {
            Arc::new(Self {
                calls: AtomicUsize::new(0),
                response,
            })
        }
    }

    impl PostHandler for MockHandler {
        fn handle_post(
            self: Arc<Self>,
            _parts: RequestParts,
        ) -> futures::future::BoxFuture<'static, PostResponse> {
            Box::pin(async move {
                self.calls.fetch_add(1, Ordering::SeqCst);
                PostResponse {
                    status: self.response.status,
                    body: self.response.body.clone(),
                    extra_headers: self.response.extra_headers.clone(),
                }
            })
        }
    }

    fn temp_socket_path() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("sock");
        (dir, path)
    }

    fn run<F: std::future::Future<Output = T>, T>(f: F) -> T {
        smol::block_on(f)
    }

    fn well_formed_post(nonce: &str, accept: &str, content_type: &str, body: &str) -> Vec<u8> {
        let mut req = format!(
            "POST /mcp HTTP/1.1\r\n\
             Host: localhost\r\n\
             Authorization: Nonce {nonce}\r\n\
             X-Copilot-Session-Id: test-session\r\n\
             Accept: {accept}\r\n\
             Content-Type: {content_type}\r\n\
             Content-Length: {len}\r\n\
             \r\n",
            len = body.len()
        )
        .into_bytes();
        req.extend_from_slice(body.as_bytes());
        req
    }

    #[test]
    fn returns_401_on_missing_auth() {
        run(async {
            let (_dir, path) = temp_socket_path();
            let handler = MockHandler::new(PostResponse::accepted());
            let _server = Server::bind(path.clone(), "nonce".into(), SessionStore::new(), handler)
                .unwrap();

            let req = b"POST /mcp HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nContent-Length: 2\r\n\r\n{}";
            let resp = send_and_read(&path, req).await;
            assert_eq!(parse_status(&resp), 401);
        });
    }

    #[test]
    fn returns_401_on_wrong_auth() {
        run(async {
            let (_dir, path) = temp_socket_path();
            let handler = MockHandler::new(PostResponse::accepted());
            let _server = Server::bind(path.clone(), "secret".into(), SessionStore::new(), handler).unwrap();

            let req = well_formed_post(
                "wrong-nonce",
                "application/json, text/event-stream",
                "application/json",
                "{}",
            );
            let resp = send_and_read(&path, &req).await;
            assert_eq!(parse_status(&resp), 401);
        });
    }

    #[test]
    fn returns_404_on_unknown_path() {
        run(async {
            let (_dir, path) = temp_socket_path();
            let handler = MockHandler::new(PostResponse::accepted());
            let _server = Server::bind(path.clone(), "n".into(), SessionStore::new(), handler).unwrap();

            let req = b"GET /nope HTTP/1.1\r\nHost: x\r\nAuthorization: Nonce n\r\nAccept: text/event-stream\r\n\r\n";
            let resp = send_and_read(&path, req).await;
            assert_eq!(parse_status(&resp), 404);
        });
    }

    #[test]
    fn returns_405_on_unsupported_method_for_mcp() {
        run(async {
            let (_dir, path) = temp_socket_path();
            let handler = MockHandler::new(PostResponse::accepted());
            let _server = Server::bind(path.clone(), "n".into(), SessionStore::new(), handler).unwrap();

            let req = b"PATCH /mcp HTTP/1.1\r\nHost: x\r\nAuthorization: Nonce n\r\nContent-Length: 0\r\n\r\n";
            let resp = send_and_read(&path, req).await;
            assert_eq!(parse_status(&resp), 405);
        });
    }

    #[test]
    fn returns_415_on_post_with_wrong_content_type() {
        run(async {
            let (_dir, path) = temp_socket_path();
            let handler = MockHandler::new(PostResponse::accepted());
            let _server = Server::bind(path.clone(), "n".into(), SessionStore::new(), handler).unwrap();

            let req = well_formed_post(
                "n",
                "application/json, text/event-stream",
                "text/plain",
                "hi",
            );
            let resp = send_and_read(&path, &req).await;
            assert_eq!(parse_status(&resp), 415);
        });
    }

    #[test]
    fn returns_406_when_post_does_not_accept_event_stream() {
        run(async {
            let (_dir, path) = temp_socket_path();
            let handler = MockHandler::new(PostResponse::accepted());
            let _server = Server::bind(path.clone(), "n".into(), SessionStore::new(), handler).unwrap();

            // Accept lacks text/event-stream.
            let req = well_formed_post("n", "application/json", "application/json", "{}");
            let resp = send_and_read(&path, &req).await;
            assert_eq!(parse_status(&resp), 406);
        });
    }

    #[test]
    fn post_handler_invoked_on_well_formed_request() {
        run(async {
            let (_dir, path) = temp_socket_path();
            let handler = MockHandler::new(PostResponse::json(StatusCode::OK, br#"{"ok":true}"#.to_vec()));
            let _server = Server::bind(path.clone(), "n".into(), SessionStore::new(), handler.clone()).unwrap();

            let req = well_formed_post(
                "n",
                "application/json, text/event-stream",
                "application/json",
                r#"{"method":"ping","id":1}"#,
            );
            let resp = send_and_read(&path, &req).await;
            assert_eq!(parse_status(&resp), 200);
            assert!(
                std::str::from_utf8(&resp).unwrap().contains(r#"{"ok":true}"#),
                "body must include the handler's payload"
            );
            assert_eq!(handler.calls.load(Ordering::SeqCst), 1);
        });
    }

    #[test]
    fn delete_with_known_session_returns_200() {
        run(async {
            let (_dir, path) = temp_socket_path();
            let store = SessionStore::new();
            store
                .try_create("test-session".into(), "2025-11-25".into(), None, None)
                .unwrap();
            let handler = MockHandler::new(PostResponse::accepted());
            let _server = Server::bind(path.clone(), "n".into(), store.clone(), handler).unwrap();

            let req = b"DELETE /mcp HTTP/1.1\r\nHost: x\r\nAuthorization: Nonce n\r\nX-Copilot-Session-Id: test-session\r\n\r\n";
            let resp = send_and_read(&path, req).await;
            assert_eq!(parse_status(&resp), 200);
            assert!(!store.exists("test-session"));
        });
    }

    #[test]
    fn delete_with_unknown_session_returns_404() {
        run(async {
            let (_dir, path) = temp_socket_path();
            let handler = MockHandler::new(PostResponse::accepted());
            let _server = Server::bind(path.clone(), "n".into(), SessionStore::new(), handler).unwrap();

            let req = b"DELETE /mcp HTTP/1.1\r\nHost: x\r\nAuthorization: Nonce n\r\nX-Copilot-Session-Id: ghost\r\n\r\n";
            let resp = send_and_read(&path, req).await;
            assert_eq!(parse_status(&resp), 404);
        });
    }

    #[test]
    fn get_attaches_sse_and_pumps_broadcast_frames() {
        run(async {
            let (_dir, path) = temp_socket_path();
            let store = SessionStore::new();
            store
                .try_create("test-session".into(), "2025-11-25".into(), None, None)
                .unwrap();
            let handler = MockHandler::new(PostResponse::accepted());
            let _server = Server::bind(path.clone(), "n".into(), store.clone(), handler).unwrap();

            // Open the GET stream, then broadcast.
            let mut client = smol::net::unix::UnixStream::connect(&path).await.unwrap();
            client
                .write_all(
                    b"GET /mcp HTTP/1.1\r\nHost: x\r\nAuthorization: Nonce n\r\nAccept: text/event-stream\r\nX-Copilot-Session-Id: test-session\r\n\r\n",
                )
                .await
                .unwrap();

            // Give the server a moment to attach the SSE stream before we
            // broadcast; otherwise the broadcast races the attach.
            for _ in 0..20 {
                smol::Timer::after(std::time::Duration::from_millis(10)).await;
                if !store.send_to("test-session", "ignored-warmup".into()) {
                    // Not yet attached.
                    continue;
                }
                break;
            }
            store.broadcast(r#"{"jsonrpc":"2.0","method":"hello","params":{}}"#.into());

            // Read enough bytes for headers + frame.
            let mut accumulated = Vec::new();
            let mut buf = [0u8; 1024];
            for _ in 0..50 {
                match smol::future::or(
                    async { client.read(&mut buf).await.map_err(|e| e.to_string()) },
                    async {
                        smol::Timer::after(std::time::Duration::from_millis(100)).await;
                        Err("timeout".into())
                    },
                )
                .await
                {
                    Ok(0) => break,
                    Ok(n) => {
                        accumulated.extend_from_slice(&buf[..n]);
                        if accumulated.windows(7).any(|w| w == b"\"hello\"") {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            let body = String::from_utf8_lossy(&accumulated);
            assert!(
                body.contains("text/event-stream"),
                "expected SSE response headers, got: {body}"
            );
            assert!(
                body.contains(r#""method":"hello""#),
                "expected the broadcast frame, got: {body}"
            );
        });
    }

    #[test]
    fn get_on_unknown_session_returns_404() {
        run(async {
            let (_dir, path) = temp_socket_path();
            let handler = MockHandler::new(PostResponse::accepted());
            let _server = Server::bind(path.clone(), "n".into(), SessionStore::new(), handler).unwrap();

            let req = b"GET /mcp HTTP/1.1\r\nHost: x\r\nAuthorization: Nonce n\r\nAccept: text/event-stream\r\nX-Copilot-Session-Id: ghost\r\n\r\n";
            let resp = send_and_read(&path, req).await;
            assert_eq!(parse_status(&resp), 404);
        });
    }

    #[test]
    fn second_get_returns_409_when_already_attached() {
        run(async {
            let (_dir, path) = temp_socket_path();
            let store = SessionStore::new();
            store
                .try_create("test-session".into(), "2025-11-25".into(), None, None)
                .unwrap();
            let handler = MockHandler::new(PostResponse::accepted());
            let _server = Server::bind(path.clone(), "n".into(), store.clone(), handler).unwrap();

            // First GET: attach and hold.
            let mut first = smol::net::unix::UnixStream::connect(&path).await.unwrap();
            first
                .write_all(b"GET /mcp HTTP/1.1\r\nHost: x\r\nAuthorization: Nonce n\r\nAccept: text/event-stream\r\nX-Copilot-Session-Id: test-session\r\n\r\n")
                .await
                .unwrap();

            // Wait until attach is observable.
            for _ in 0..20 {
                smol::Timer::after(std::time::Duration::from_millis(10)).await;
                if store.send_to("test-session", "warmup".into()) {
                    break;
                }
            }

            // Second GET should 409.
            let req = b"GET /mcp HTTP/1.1\r\nHost: x\r\nAuthorization: Nonce n\r\nAccept: text/event-stream\r\nX-Copilot-Session-Id: test-session\r\n\r\n";
            let resp = send_and_read(&path, req).await;
            assert_eq!(parse_status(&resp), 409);

            // Keep `first` alive until the assert is done.
            drop(first);
        });
    }

    #[test]
    fn format_sse_chunk_emits_chunked_transfer_frame() {
        let chunk = format_sse_chunk(r#"{"a":1}"#);
        let s = String::from_utf8(chunk).unwrap();
        // payload = "data: {\"a\":1}\r\n\r\n" (17 bytes = 0x11 hex)
        assert!(s.starts_with("11\r\n"), "size line wrong: {s:?}");
        assert!(s.contains("data: {\"a\":1}\r\n\r\n"));
        assert!(s.ends_with("\r\n"));
    }

    #[test]
    fn constant_time_eq_basics() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
        assert!(!constant_time_eq(b"", b"a"));
        assert!(constant_time_eq(b"", b""));
    }

}
