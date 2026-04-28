use std::net::Ipv4Addr;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use editor_capabilities::EditorCapabilities;
use gpui::{App, AppContext as _, Task};
use smol::net::TcpListener;

use crate::mcp::McpDispatcher;

/// A WebSocket server bound to 127.0.0.1 on an OS-assigned port. Accepts
/// connections from `claude` CLI instances spawned with the matching
/// `CLAUDE_CODE_SSE_PORT` env var; validates the auth token; dispatches MCP
/// JSON-RPC method calls into the [`EditorCapabilities`] surface.
///
/// Lifecycle is owned by [`crate::ClaudeCodeAttachment`]. The accept loop
/// runs as a background task; dropping the task aborts it. The dispatcher
/// itself is foreground-only because [`EditorCapabilities`] holds GPUI
/// entities that are not `Send`; the accept loop is responsible only for
/// TCP+WS plumbing and forwards parsed MCP calls to the foreground.
pub struct Server {
    port: u16,
    // Used once the WS dispatch path is wired up.
    #[allow(dead_code)]
    auth_token: String,
    #[allow(dead_code)]
    _dispatcher: McpDispatcher,
    _accept_task: Task<()>,
}

impl Server {
    pub fn port(&self) -> u16 {
        self.port
    }

    #[allow(dead_code)]
    pub fn auth_token(&self) -> &str {
        &self.auth_token
    }

    /// Bind to 127.0.0.1:0 and start accepting connections. Returns the bound
    /// server (with its OS-assigned port readable via [`Server::port`]).
    pub fn bind(
        auth_token: String,
        capabilities: Arc<dyn EditorCapabilities>,
        cx: &mut App,
    ) -> Result<Self> {
        let listener = smol::block_on(TcpListener::bind((Ipv4Addr::LOCALHOST, 0)))
            .context("binding Claude /ide WebSocket listener to 127.0.0.1:0")?;
        let port = listener.local_addr()?.port();

        let dispatcher = McpDispatcher::new(capabilities);
        let auth_token_for_task = auth_token.clone();
        let accept_task = cx.background_spawn(async move {
            run_accept_loop(listener, auth_token_for_task).await;
        });

        Ok(Self {
            port,
            auth_token,
            _dispatcher: dispatcher,
            _accept_task: accept_task,
        })
    }
}

async fn run_accept_loop(listener: TcpListener, auth_token: String) {
    loop {
        let (_stream, addr) = match listener.accept().await {
            Ok(connection) => connection,
            Err(error) => {
                log::warn!("Claude /ide accept failed: {error}");
                continue;
            }
        };

        // Reject anything that isn't loopback. Even though we bound to
        // 127.0.0.1, defense-in-depth.
        if !addr.ip().is_loopback() {
            log::warn!("rejecting non-loopback Claude /ide connection from {addr}");
            continue;
        }

        // WS upgrade + auth handshake + JSON-RPC dispatch are wired up in a
        // follow-up commit. The lockfile/port allocation that this commit
        // ships is independent of the protocol layer.
        log::debug!(
            "Claude /ide connection from {addr} (auth={}, dispatch awaiting follow-up)",
            mask_token(&auth_token)
        );
    }
}

fn mask_token(token: &str) -> String {
    if token.len() < 6 {
        "***".to_string()
    } else {
        format!("{}…{}", &token[..2], &token[token.len() - 2..])
    }
}
