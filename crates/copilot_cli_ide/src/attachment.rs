//! Lifecycle for one Copilot CLI `/ide` attachment.
//!
//! Built when an `ai_terminal_panel` spawns a `copilot` child. Holds the
//! Unix-socket server, its lockfile, the MCP dispatcher (foreground), the
//! `SessionStore`, the broadcaster, and the selection / diagnostics
//! observers. Drop tears everything down: the lockfile is unlinked, the
//! accept loop is cancelled, in-flight connections are aborted, the socket
//! file is removed with the temp dir, the foreground dispatcher task ends.
//!
//! Unlike Claude's attachment, **no env vars are returned to inject into the
//! spawned child**: Copilot CLI's auto-connect is purely lockfile-driven —
//! it scans `~/.copilot/ide/*.lock` and matches the `workspaceFolders` field
//! against its current working directory.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result};
use editor_capabilities::{DiagnosticInfo, EditorCapabilities, EditorSelection};
use gpui::{App, AppContext as _, Entity, EntityId, Subscription, Task};
use parking_lot::Mutex;

use crate::broadcaster::Broadcaster;
use crate::lockfile::{self, Lockfile, LockfileGuard};
use crate::mcp::{McpDispatcher, McpPostHandler};
use crate::router::{CopilotTerminalRouter, TerminalRouter};
use crate::transport::{Server, SessionStore};

const SELECTION_DEBOUNCE: Duration = Duration::from_millis(200);
const DIAGNOSTICS_DEBOUNCE: Duration = Duration::from_millis(200);

#[derive(Default)]
struct SelectionDebounceState {
    latest: Option<Option<EditorSelection>>,
    task: Option<Task<()>>,
}

#[derive(Default)]
struct DiagnosticsDebounceState {
    /// Path → most-recent diagnostics for that path. Each callback overwrites
    /// the entry (the latest snapshot wins). The debounced flush sends every
    /// entry as one `diagnostics_changed` notification, then clears.
    entries: HashMap<Arc<Path>, Vec<DiagnosticInfo>>,
    task: Option<Task<()>>,
}

pub struct CopilotAttachment {
    socket_path: PathBuf,
    nonce: String,
    workspace_root: PathBuf,
    /// Current `workspaceFolders` set persisted in the lockfile. Tracked so
    /// `refresh_workspace_folders` can short-circuit when the user's visible
    /// worktree set hasn't changed since the last call.
    current_workspace_folders: Vec<PathBuf>,
    // Keep-alives in tear-down order: socket dir + lockfile (outside-world
    // side effects) → server + dispatcher → broadcaster + observers →
    // capabilities. Each field's Drop runs in declaration order, so the
    // outside-world bits drop first.
    _socket_dir: tempfile::TempDir,
    _lockfile_guard: LockfileGuard,
    _server: Server,
    _sessions: SessionStore,
    /// Per-attachment routing map (mcp-session-id → ancestor PID → terminal
    /// EntityId). Shared with the dispatcher via `Rc`. Same-workspace
    /// terminals all register against this single router because we now
    /// share one attachment per workspace.
    router: Rc<CopilotTerminalRouter>,
    _dispatcher: McpDispatcher,
    _broadcaster: Broadcaster,
    _selection_subscription: Subscription,
    _diagnostics_subscription: Subscription,
    _capabilities: Arc<dyn EditorCapabilities>,
}

impl CopilotAttachment {
    pub fn prepare(
        workspace_root: PathBuf,
        capabilities: Arc<dyn EditorCapabilities>,
        cx: &mut App,
    ) -> Result<Entity<Self>> {
        // Sweep stale lockfiles from prior crashed Zerminal processes.
        // Best-effort; failures here are logged but don't block.
        let state_dir = lockfile::copilot_state_dir()
            .context("resolving Copilot CLI state dir")?;
        match lockfile::sweep_stale(&state_dir) {
            Ok(removed) if !removed.is_empty() => {
                log::info!(
                    "Copilot /ide swept {} stale lockfile(s) before attachment",
                    removed.len()
                );
            }
            Ok(_) => {}
            Err(error) => {
                log::warn!("Copilot /ide stale-lockfile sweep failed: {error:#}");
            }
        }

        // Bind Unix socket under a fresh tempdir so the socket file gets
        // removed when the dir is dropped. macOS / Linux only.
        let socket_dir = tempfile::Builder::new()
            .prefix("zerminal-copilot-")
            .tempdir()
            .context("creating socket tempdir")?;
        let socket_path = socket_dir.path().join("sock");

        let nonce = uuid::Uuid::new_v4().to_string();
        let sessions = SessionStore::new();
        let broadcaster = Broadcaster::new(sessions.clone());
        // Router shares this attachment's SessionStore so its session_id →
        // client_pid lookup hits the same store the dispatcher writes to on
        // initialize. Built once per attachment; cloned `Rc` into both the
        // dispatcher (foreground task) and the attachment (foreground entity).
        let router: Rc<CopilotTerminalRouter> = Rc::new(CopilotTerminalRouter::new(sessions.clone()));
        let dispatcher = McpDispatcher::spawn(
            capabilities.clone(),
            router.clone() as Rc<dyn TerminalRouter>,
            cx,
        );
        let post_handler = Arc::new(McpPostHandler::new(
            sessions.clone(),
            dispatcher.sender(),
        ));
        let server = Server::bind(
            socket_path.clone(),
            nonce.clone(),
            sessions.clone(),
            post_handler,
        )
        .context("binding Unix socket")?;

        // 200ms-debounced selection observer. The `Fn` callback fires on
        // every cursor / selection change; we stash the latest selection
        // and arm a single delayed flush.
        let executor = cx.background_executor().clone();
        let selection_debounce = Arc::new(Mutex::new(SelectionDebounceState::default()));
        let selection_subscription = capabilities.observe_selection(
            Box::new({
                let broadcaster = broadcaster.clone();
                let executor = executor.clone();
                move |selection, cx| {
                    let mut state = selection_debounce.lock();
                    state.latest = Some(selection);
                    if state.task.is_some() {
                        return;
                    }
                    let broadcaster = broadcaster.clone();
                    let executor = executor.clone();
                    let debounce = selection_debounce.clone();
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

        // 200ms-debounced diagnostics observer. The callback gives us paths
        // that changed; we synchronously fetch current diagnostics for each
        // (foreground access is required) and stash them. The debounced
        // flush then broadcasts the merged batch.
        let diagnostics_debounce = Arc::new(Mutex::new(DiagnosticsDebounceState::default()));
        let diagnostics_subscription = capabilities.observe_diagnostics(
            Box::new({
                let broadcaster = broadcaster.clone();
                let executor = executor.clone();
                let capabilities_for_fetch = capabilities.clone();
                move |changed_paths, cx| {
                    let mut state = diagnostics_debounce.lock();
                    for path in changed_paths {
                        let entries = capabilities_for_fetch.get_diagnostics(Some(path.clone()), cx);
                        state.entries.insert(path, entries);
                    }
                    if state.task.is_some() {
                        return;
                    }
                    let broadcaster = broadcaster.clone();
                    let executor = executor.clone();
                    let debounce = diagnostics_debounce.clone();
                    state.task = Some(cx.background_spawn(async move {
                        executor.timer(DIAGNOSTICS_DEBOUNCE).await;
                        let entries: Vec<(Arc<Path>, Vec<DiagnosticInfo>)> = {
                            let mut state = debounce.lock();
                            state.task = None;
                            state.entries.drain().collect()
                        };
                        if !entries.is_empty() {
                            broadcaster.send_diagnostics_changed(entries);
                        }
                    }));
                }
            }),
            cx,
        );

        // Build the lockfile pointing to our socket. The `workspaceFolders`
        // list is what Copilot CLI uses to match this lockfile against its
        // current working directory; supply every visible worktree root so
        // the user can `cd` into any of them and have auto-connect work.
        let workspace_folders: Vec<PathBuf> = capabilities
            .list_workspace_folders(cx)
            .into_iter()
            .map(|p| p.to_path_buf())
            .collect();
        let workspace_folders = if workspace_folders.is_empty() {
            vec![workspace_root.clone()]
        } else {
            workspace_folders
        };
        let lockfile = Lockfile::new(
            socket_path.to_string_lossy().into_owned(),
            &nonce,
            workspace_folders.clone(),
        );
        let lockfile_guard = lockfile::write_atomic(&state_dir, &lockfile)
            .context("writing Copilot lockfile")?;
        log::info!(
            "Copilot /ide attachment ready: socket={} lockfile={} workspace_root={}",
            socket_path.display(),
            lockfile_guard.path().display(),
            workspace_root.display()
        );

        let entity = cx.new(move |_cx| Self {
            socket_path,
            nonce,
            workspace_root,
            current_workspace_folders: workspace_folders,
            _socket_dir: socket_dir,
            _lockfile_guard: lockfile_guard,
            _server: server,
            _sessions: sessions,
            router,
            _dispatcher: dispatcher,
            _broadcaster: broadcaster,
            _selection_subscription: selection_subscription,
            _diagnostics_subscription: diagnostics_subscription,
            _capabilities: capabilities,
        });
        Ok(entity)
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    pub fn nonce(&self) -> &str {
        &self.nonce
    }

    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    /// Register a terminal's PTY-child PID against its `EntityId`. The panel
    /// calls this immediately after spawning a `copilot` terminal so the
    /// dispatcher can route per-terminal tool calls (`update_session_name`,
    /// `close_diff`) back to the originating tab.
    ///
    /// The shared-attachment model means many terminals in the same workspace
    /// register against the same router — that's the point.
    pub fn register_terminal(&self, pid: u32, entity_id: EntityId) {
        self.router.register(pid, entity_id);
    }

    /// Rewrite the lockfile's `workspaceFolders` list in place if the new
    /// set differs from the currently-persisted one. Same socket, same
    /// nonce, same lockfile path — only the JSON `workspaceFolders` array
    /// changes.
    ///
    /// Why we need this: under the shared-attachment model, the first
    /// terminal in a workspace writes the lockfile with whatever worktrees
    /// were visible at the time. If the user later opens a new worktree and
    /// then spawns another `copilot` terminal whose cwd is in the new
    /// worktree, the CLI's lockfile scan won't match — auto-connect
    /// silently fails. Calling this from the panel on each Copilot spawn
    /// keeps the lockfile fresh.
    pub fn refresh_workspace_folders(&mut self, folders: Vec<PathBuf>) -> Result<()> {
        let normalized = if folders.is_empty() {
            vec![self.workspace_root.clone()]
        } else {
            folders
        };
        if normalized == self.current_workspace_folders {
            return Ok(());
        }
        let lockfile = Lockfile::new(
            self.socket_path.to_string_lossy().into_owned(),
            &self.nonce,
            normalized.clone(),
        );
        lockfile::write_atomic_to_path(self._lockfile_guard.path(), &lockfile)
            .context("rewriting Copilot lockfile with refreshed workspaceFolders")?;
        log::info!(
            "Copilot /ide attachment lockfile refreshed: workspace_folders={normalized:?}",
        );
        self.current_workspace_folders = normalized;
        Ok(())
    }

    /// Reverse of [`register_terminal`]. Verifies `entity_id` matches the
    /// current map entry before removing — guards against the close-then-
    /// PID-reuse race where a closing terminal's late cleanup would
    /// otherwise evict a freshly-spawned terminal that grabbed the same PID.
    pub fn unregister_terminal(&self, pid: u32, entity_id: EntityId) {
        self.router.unregister(pid, entity_id);
    }
}
