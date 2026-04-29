use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use editor_capabilities::EditorCapabilities;
use gpui::{App, AppContext as _, Context, Entity};

use crate::broadcaster::Broadcaster;
use crate::lockfile::{self, Lockfile, LockfileGuard};
use crate::mcp::McpDispatcher;
use crate::server::Server;

/// One Claude `/ide` attachment, hosting a per-pane WebSocket server. Owns
/// the bound port, lockfile, and accept task. Drop unlinks the lockfile and
/// aborts the accept task.
///
/// Lifecycle:
///   1. [`ClaudeCodeAttachment::prepare`] is called before the terminal is
///      spawned. It allocates the port, generates an auth token, writes the
///      lockfile, and returns the env vars to inject into the terminal's
///      child process.
///   2. The caller spawns the terminal (with the returned env merged into
///      `SpawnInTerminal.env`).
///   3. The Claude CLI auto-reads `CLAUDE_CODE_SSE_PORT` and connects to the
///      WS server on loopback.
pub struct ClaudeCodeAttachment {
    port: u16,
    auth_token: String,
    workspace_root: PathBuf,
    state: AttachmentState,
    _capabilities: Arc<dyn EditorCapabilities>,
    _dispatcher: McpDispatcher,
    _broadcaster: Broadcaster,
    _lockfile_guard: LockfileGuard,
    _server: Server,
}

#[derive(Clone, Debug)]
pub enum AttachmentState {
    AwaitingClient,
    Connected,
    Disconnected { reason: String },
}

impl ClaudeCodeAttachment {
    /// Bind a WebSocket server, write the lockfile, and return the entity
    /// plus the env map to inject into the spawned terminal.
    pub fn prepare(
        workspace_root: PathBuf,
        capabilities: Arc<dyn EditorCapabilities>,
        cx: &mut App,
    ) -> Result<(Entity<Self>, HashMap<String, String>)> {
        let auth_token = uuid::Uuid::new_v4().to_string();
        let broadcaster = Broadcaster::new();
        let dispatcher = McpDispatcher::spawn(capabilities.clone(), broadcaster.clone(), cx);
        let server = Server::bind(
            auth_token.clone(),
            dispatcher.sender(),
            broadcaster.clone(),
            cx,
        )?;
        let port = server.port();

        let lockfile = Lockfile::new(vec![workspace_root.clone()], auth_token.clone());
        let lockfile_guard = lockfile::write_atomic(port, &lockfile)?;

        let env = HashMap::from([
            ("CLAUDE_CODE_SSE_PORT".to_string(), port.to_string()),
            ("ENABLE_IDE_INTEGRATION".to_string(), "true".to_string()),
        ]);

        let entity = cx.new(move |_cx| Self {
            port,
            auth_token,
            workspace_root,
            state: AttachmentState::AwaitingClient,
            _capabilities: capabilities,
            _dispatcher: dispatcher,
            _broadcaster: broadcaster,
            _lockfile_guard: lockfile_guard,
            _server: server,
        });

        Ok((entity, env))
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn auth_token(&self) -> &str {
        &self.auth_token
    }

    pub fn workspace_root(&self) -> &PathBuf {
        &self.workspace_root
    }

    pub fn state(&self) -> &AttachmentState {
        &self.state
    }

    pub fn shutdown(&mut self, _cx: &mut Context<Self>) {
        self.state = AttachmentState::Disconnected {
            reason: "shutdown requested".to_string(),
        };
        // Dropping `_server` and `_lockfile_guard` happens when the entity
        // itself is dropped; nothing more to do here for now.
    }
}
