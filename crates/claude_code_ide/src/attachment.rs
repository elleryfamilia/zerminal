use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use editor_capabilities::{EditorCapabilities, EditorSelection};
use gpui::{App, AppContext as _, Context, Entity, Subscription, Task};
use parking_lot::Mutex;

use crate::broadcaster::Broadcaster;
use crate::lockfile::{self, Lockfile, LockfileGuard};
use crate::mcp::McpDispatcher;
use crate::server::Server;

const SELECTION_DEBOUNCE: Duration = Duration::from_millis(100);

#[derive(Default)]
struct DebounceState {
    latest: Option<Option<EditorSelection>>,
    task: Option<Task<()>>,
}

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
    _selection_subscription: Subscription,
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

        // Live cursor/selection observer: every editor change fans out a
        // selection_changed notification to all connected Claude clients.
        // Without this, Claude only sees the snapshot pushed on `tools/list`,
        // and "what am I selecting?" stays stale forever.
        //
        // Debounced to 100ms to match claudecode.nvim — Editor's
        // SelectionsChanged event fires on every keystroke and drag
        // micro-update, which would otherwise flood the WebSocket with
        // dozens of frames per second.
        let debounce: Arc<Mutex<DebounceState>> = Arc::new(Mutex::new(DebounceState::default()));
        let executor = cx.background_executor().clone();
        let selection_subscription = capabilities.observe_selection(
            Box::new({
                let broadcaster = broadcaster.clone();
                move |selection, cx| {
                    let mut state = debounce.lock();
                    state.latest = Some(selection);
                    if state.task.is_some() {
                        return;
                    }
                    let broadcaster = broadcaster.clone();
                    let debounce = debounce.clone();
                    let executor = executor.clone();
                    state.task = Some(cx.background_spawn(async move {
                        executor.timer(SELECTION_DEBOUNCE).await;
                        let to_send = {
                            let mut state = debounce.lock();
                            state.task = None;
                            state.latest.take()
                        };
                        if let Some(selection) = to_send {
                            broadcaster.send_selection_changed(selection.as_ref());
                        }
                    }));
                }
            }),
            cx,
        );

        let lockfile = Lockfile::new(vec![workspace_root.clone()], auth_token.clone());
        let lockfile_guard = lockfile::write_atomic(port, &lockfile)?;

        // FORCE_CODE_TERMINAL makes Claude v2.1.122's `bF()` return true, which
        // enables the auto-connect path that wires the IDE into `mcpServers.ide`.
        // Without it, the MCP connection still happens (we hold a lockfile and a
        // matching CLAUDE_CODE_SSE_PORT) but the client gets registered as a
        // regular MCP server — not the special "ide" client — so the
        // `selection_changed` notification handler is never registered. See
        // memory/project_claude_ide_protocol.md for the full chain. Setting
        // CLAUDE_CODE_AUTO_CONNECT_IDE=true is belt-and-suspenders against config
        // toggles that disable the default.
        let env = HashMap::from([
            ("CLAUDE_CODE_SSE_PORT".to_string(), port.to_string()),
            ("ENABLE_IDE_INTEGRATION".to_string(), "true".to_string()),
            ("FORCE_CODE_TERMINAL".to_string(), "true".to_string()),
            ("CLAUDE_CODE_AUTO_CONNECT_IDE".to_string(), "true".to_string()),
        ]);

        let entity = cx.new(move |_cx| Self {
            port,
            auth_token,
            workspace_root,
            state: AttachmentState::AwaitingClient,
            _capabilities: capabilities,
            _dispatcher: dispatcher,
            _broadcaster: broadcaster,
            _selection_subscription: selection_subscription,
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
