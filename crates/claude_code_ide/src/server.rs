use std::net::Ipv4Addr;

use anyhow::{Context as _, Result};
use async_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use async_tungstenite::tungstenite::http::StatusCode;
use async_tungstenite::tungstenite::{Message as WebSocketMessage, error::Error as WsError};
use futures::channel::oneshot;
use futures::StreamExt as _;
use gpui::{App, AppContext as _, Task};
use serde_json::{Value, json};
use smol::net::{TcpListener, TcpStream};

use crate::mcp::{McpCall, McpCallSender};

/// HTTP request header the Claude CLI uses to authenticate to /ide.
/// Mirrors the convention adopted by claudecode.nvim and similar editors.
const AUTH_HEADER: &str = "x-claude-code-ide-authorization";

/// A WebSocket server bound to 127.0.0.1 on an OS-assigned port. Accepts
/// connections from `claude` CLI instances spawned with the matching
/// `CLAUDE_CODE_SSE_PORT` env var, validates the auth header, and forwards
/// MCP JSON-RPC method calls to the foreground dispatcher.
///
/// Lifecycle is owned by [`crate::ClaudeCodeAttachment`]. The accept loop
/// runs as a background task; per-connection handlers run as nested
/// background tasks. Dropping [`Server`] aborts everything.
pub struct Server {
    port: u16,
    _accept_task: Task<()>,
}

impl Server {
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Bind to 127.0.0.1:0 and start accepting connections. Each accepted
    /// connection forwards parsed MCP calls into `dispatcher_sender`.
    pub fn bind(
        auth_token: String,
        dispatcher_sender: McpCallSender,
        cx: &mut App,
    ) -> Result<Self> {
        let listener = smol::block_on(TcpListener::bind((Ipv4Addr::LOCALHOST, 0)))
            .context("binding Claude /ide WebSocket listener to 127.0.0.1:0")?;
        let port = listener.local_addr()?.port();

        let executor = cx.background_executor().clone();
        let accept_task = cx.background_spawn(async move {
            run_accept_loop(listener, auth_token, dispatcher_sender, executor).await;
        });

        Ok(Self {
            port,
            _accept_task: accept_task,
        })
    }
}

async fn run_accept_loop(
    listener: TcpListener,
    auth_token: String,
    dispatcher_sender: McpCallSender,
    executor: gpui::BackgroundExecutor,
) {
    let local = listener
        .local_addr()
        .map(|addr| addr.to_string())
        .unwrap_or_else(|_| "<unknown>".to_string());
    log::info!("Claude /ide accept loop ready on {local}");
    loop {
        let (stream, addr) = match listener.accept().await {
            Ok(connection) => connection,
            Err(error) => {
                log::warn!("Claude /ide accept failed: {error}");
                continue;
            }
        };

        log::info!("Claude /ide TCP accept from {addr}");

        if !addr.ip().is_loopback() {
            log::warn!("rejecting non-loopback Claude /ide connection from {addr}");
            continue;
        }

        let auth_token = auth_token.clone();
        let dispatcher_sender = dispatcher_sender.clone();
        executor
            .spawn(async move {
                log::info!("Claude /ide WebSocket handshake starting for {addr}");
                if let Err(error) =
                    handle_connection(stream, &auth_token, dispatcher_sender).await
                {
                    log::warn!("Claude /ide connection from {addr} ended with error: {error:#}");
                } else {
                    log::info!("Claude /ide connection from {addr} closed cleanly");
                }
            })
            .detach();
    }
}

async fn handle_connection(
    stream: TcpStream,
    expected_token: &str,
    dispatcher_sender: McpCallSender,
) -> Result<()> {
    let expected_token = expected_token.to_string();
    let ws_stream = async_tungstenite::accept_hdr_async(stream, AuthCallback { expected_token })
        .await
        .context("WebSocket handshake")?;
    let (mut sink, mut source) = ws_stream.split();

    while let Some(message) = source.next().await {
        let message = match message {
            Ok(message) => message,
            Err(WsError::ConnectionClosed | WsError::AlreadyClosed) => break,
            Err(error) => return Err(anyhow::anyhow!("WebSocket read error: {error}")),
        };

        match message {
            WebSocketMessage::Text(text) => {
                let text_str: &str = text.as_ref();
                if let Some(response) = handle_text_frame(text_str, &dispatcher_sender).await {
                    sink.send(WebSocketMessage::Text(response.into())).await?;
                }
            }
            WebSocketMessage::Ping(payload) => {
                sink.send(WebSocketMessage::Pong(payload)).await?;
            }
            WebSocketMessage::Close(_) => break,
            WebSocketMessage::Binary(_) | WebSocketMessage::Pong(_) | WebSocketMessage::Frame(_) => {
                // Ignore — Claude /ide is text-only JSON-RPC.
            }
        }
    }
    Ok(())
}

/// Parse one JSON-RPC text frame, dispatch via the foreground sender, and
/// build a response frame. Returns None for valid notifications (no id) so
/// the caller doesn't reply.
async fn handle_text_frame(text: &str, sender: &McpCallSender) -> Option<String> {
    let request: Value = match serde_json::from_str(text) {
        Ok(value) => value,
        Err(error) => {
            return Some(error_frame(Value::Null, -32700, format!("parse error: {error}")));
        }
    };

    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let method = match request.get("method").and_then(Value::as_str) {
        Some(method) => method.to_string(),
        None => {
            return Some(error_frame(id, -32600, "missing method".to_string()));
        }
    };
    let params = request.get("params").cloned().unwrap_or(Value::Null);
    let is_notification = request.get("id").is_none();

    // Notifications other than ones we explicitly handle are dropped without
    // dispatching — saves the dispatcher loop a noisy "unknown method" warning
    // for things like `ide_connected` that Claude pushes after the handshake.
    if is_notification && !is_known_notification(&method) {
        log::trace!("Claude /ide ignoring unknown notification: {method}");
        return None;
    }

    let (respond_tx, respond_rx) = oneshot::channel();
    let call = McpCall {
        method: method.clone(),
        params,
        respond_to: respond_tx,
    };
    if sender.unbounded_send(call).is_err() {
        if is_notification {
            return None;
        }
        return Some(error_frame(id, -32603, "dispatcher unavailable".to_string()));
    }

    let result = match respond_rx.await {
        Ok(result) => result,
        Err(_) => {
            if is_notification {
                return None;
            }
            return Some(error_frame(id, -32603, "dispatcher dropped response".to_string()));
        }
    };

    if is_notification {
        return None;
    }

    Some(match result {
        Ok(value) => json!({ "jsonrpc": "2.0", "id": id, "result": value }).to_string(),
        Err(error) => error_frame(id, -32000, format!("{error:#}")),
    })
}

fn is_known_notification(method: &str) -> bool {
    matches!(method, "notifications/initialized" | "initialized")
}

fn error_frame(id: Value, code: i32, message: String) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message }
    })
    .to_string()
}

struct AuthCallback {
    expected_token: String,
}

impl async_tungstenite::tungstenite::handshake::server::Callback for AuthCallback {
    fn on_request(
        self,
        request: &Request,
        response: Response,
    ) -> std::result::Result<Response, ErrorResponse> {
        let supplied = request
            .headers()
            .get(AUTH_HEADER)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("");
        if supplied == self.expected_token {
            Ok(response)
        } else {
            let mut error_response = ErrorResponse::new(None);
            *error_response.status_mut() = StatusCode::UNAUTHORIZED;
            Err(error_response)
        }
    }
}
