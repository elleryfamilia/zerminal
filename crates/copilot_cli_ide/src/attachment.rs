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
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result};
use editor_capabilities::{DiagnosticInfo, EditorCapabilities, EditorSelection};
use gpui::{App, AppContext as _, Entity, Subscription, Task};
use parking_lot::Mutex;

use crate::broadcaster::Broadcaster;
use crate::lockfile::{self, Lockfile, LockfileGuard};
use crate::mcp::{McpDispatcher, McpPostHandler};
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
    // Keep-alives in tear-down order: socket dir + lockfile (outside-world
    // side effects) → server + dispatcher → broadcaster + observers →
    // capabilities. Each field's Drop runs in declaration order, so the
    // outside-world bits drop first.
    _socket_dir: tempfile::TempDir,
    _lockfile_guard: LockfileGuard,
    _server: Server,
    _sessions: SessionStore,
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
        let dispatcher = McpDispatcher::spawn(capabilities.clone(), cx);
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
                let capabilities_for_fetch = capabilities.clone();
                move |changed_paths, cx| {
                    // Fetch diagnostics WITHOUT holding the debounce lock. If
                    // a future EditorCapabilities impl reentrantly fires
                    // observe_diagnostics from inside get_diagnostics (e.g.
                    // a wrapper that triggers a buffer reload on read), the
                    // reentrant callback would otherwise deadlock on the
                    // parking_lot mutex.
                    let fetched: Vec<(Arc<Path>, Vec<DiagnosticInfo>)> = changed_paths
                        .into_iter()
                        .map(|path| {
                            let entries = capabilities_for_fetch
                                .get_diagnostics(Some(path.clone()), cx);
                            (path, entries)
                        })
                        .collect();
                    let mut state = diagnostics_debounce.lock();
                    for (path, entries) in fetched {
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
            workspace_folders,
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
            _socket_dir: socket_dir,
            _lockfile_guard: lockfile_guard,
            _server: server,
            _sessions: sessions,
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
}
