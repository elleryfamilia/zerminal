use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result, anyhow};
use editor::{Editor, ToPoint as _};
use gpui::{
    AnyWindowHandle, App, AppContext as _, Entity, SharedString, Subscription, Task, WeakEntity,
};
use language::{Buffer, Point};
use workspace::{OpenOptions, Workspace};

#[derive(Clone, Debug)]
pub struct EditorSelection {
    pub path: Arc<Path>,
    pub start: Point,
    pub end: Point,
    pub text: Option<SharedString>,
}

#[derive(Clone, Debug)]
pub struct OpenEditorInfo {
    pub path: Arc<Path>,
    pub is_dirty: bool,
    pub is_active: bool,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DiagnosticSeverity {
    Error,
    Warning,
    Information,
    Hint,
}

pub struct DiagnosticInfo {
    pub path: Arc<Path>,
    pub start: Point,
    pub end: Point,
    pub severity: DiagnosticSeverity,
    pub message: SharedString,
    pub source: Option<SharedString>,
}

pub enum DiffDecision {
    Accept { final_text: String },
    Reject,
    Cancelled,
}

pub type SelectionCallback = Box<dyn Fn(Option<EditorSelection>, &mut App) + 'static>;

/// The editor surface that protocol connectors call into.
///
/// Designed protocol-neutral. v1 is consumed only by the Claude Code `/ide`
/// connector. A future ACP integration would reuse this surface for the
/// operations that aren't tightly coupled to ACP session bookkeeping
/// (selection, open file, diagnostics, workspace folders, save). ACP's
/// read/write file operations stay in `acp_thread::AcpThread` because they
/// are bound to action_log and shared_buffers.
pub trait EditorCapabilities: 'static {
    /// All visible worktree roots in the workspace.
    fn list_workspace_folders(&self, cx: &App) -> Vec<Arc<Path>>;

    /// All open editor tabs across every pane in the workspace, not just the active pane.
    fn list_open_editors(&self, cx: &App) -> Vec<OpenEditorInfo>;

    /// Selection in the workspace's currently-active editor, or None if the
    /// active item is not an editor.
    fn current_selection(&self, cx: &App) -> Option<EditorSelection>;

    fn open_file(&self, path: Arc<Path>, focus: bool, cx: &mut App) -> Task<Result<()>>;

    fn save_document(&self, path: Arc<Path>, cx: &mut App) -> Task<Result<()>>;

    fn check_dirty(&self, path: Arc<Path>, cx: &App) -> bool;

    fn get_diagnostics(&self, path: Option<Arc<Path>>, cx: &App) -> Vec<DiagnosticInfo>;

    /// Render a diff for user review. Resolves with the user's decision; on
    /// Accept, `final_text` reflects any in-place edits the user made before
    /// accepting. The implementation owns the diff entity's lifecycle — the
    /// caller never holds an `Entity<Diff>`.
    fn open_diff_for_review(
        &self,
        path: Arc<Path>,
        old_text: String,
        new_text: String,
        cx: &mut App,
    ) -> Task<Result<DiffDecision>>;

    /// Subscribe to selection changes. The callback fires for the workspace's
    /// currently-active editor; the implementation re-subscribes when the
    /// active editor changes. Dropping the returned `Subscription` removes
    /// the listener.
    fn observe_selection(&self, callback: SelectionCallback, cx: &mut App) -> Subscription;
}

pub struct WorkspaceEditorCapabilities {
    workspace: WeakEntity<Workspace>,
    window: AnyWindowHandle,
}

impl WorkspaceEditorCapabilities {
    pub fn new(workspace: WeakEntity<Workspace>, window: AnyWindowHandle) -> Self {
        Self { workspace, window }
    }

    pub fn workspace(&self) -> &WeakEntity<Workspace> {
        &self.workspace
    }

    fn read_workspace<R>(&self, cx: &App, f: impl FnOnce(&Workspace, &App) -> R) -> Option<R> {
        let workspace = self.workspace.upgrade()?;
        Some(f(workspace.read(cx), cx))
    }

    fn buffer_for_abs_path(&self, path: &Path, cx: &App) -> Option<Entity<Buffer>> {
        let workspace = self.workspace.upgrade()?;
        let project = workspace.read(cx).project().clone();
        project
            .read(cx)
            .opened_buffers(cx)
            .into_iter()
            .find(|buffer| buffer_abs_path(buffer, cx).as_deref() == Some(path))
    }
}

fn buffer_abs_path(buffer: &Entity<Buffer>, cx: &App) -> Option<PathBuf> {
    let file = buffer.read(cx).file()?;
    let local = file.as_local()?;
    Some(local.abs_path(cx))
}

fn editor_abs_path(editor: &Entity<Editor>, cx: &App) -> Option<PathBuf> {
    let multi_buffer = editor.read(cx).buffer();
    let buffer = multi_buffer.read(cx).as_singleton()?;
    let file = buffer.read(cx).file()?;
    let local = file.as_local()?;
    Some(local.abs_path(cx))
}

impl EditorCapabilities for WorkspaceEditorCapabilities {
    fn list_workspace_folders(&self, cx: &App) -> Vec<Arc<Path>> {
        self.read_workspace(cx, |workspace, cx| {
            workspace
                .visible_worktrees(cx)
                .map(|worktree| worktree.read(cx).abs_path())
                .collect()
        })
        .unwrap_or_default()
    }

    fn list_open_editors(&self, cx: &App) -> Vec<OpenEditorInfo> {
        let Some(workspace) = self.workspace.upgrade() else {
            return Vec::new();
        };
        let workspace = workspace.read(cx);
        let active_editor = workspace.active_item_as::<Editor>(cx);
        let active_path = active_editor
            .as_ref()
            .and_then(|editor| editor_abs_path(editor, cx));

        let mut result = Vec::new();
        for pane in workspace.panes() {
            let pane = pane.read(cx);
            for item in pane.items() {
                let Some(editor) = item.act_as::<Editor>(cx) else {
                    continue;
                };
                let Some(abs_path) = editor_abs_path(&editor, cx) else {
                    continue;
                };
                let multi_buffer = editor.read(cx).buffer();
                let Some(buffer) = multi_buffer.read(cx).as_singleton() else {
                    continue;
                };
                let is_dirty = buffer.read(cx).is_dirty();
                let is_active = active_path.as_deref() == Some(abs_path.as_path());
                result.push(OpenEditorInfo {
                    path: Arc::from(abs_path),
                    is_dirty,
                    is_active,
                });
            }
        }
        result
    }

    fn current_selection(&self, cx: &App) -> Option<EditorSelection> {
        let workspace = self.workspace.upgrade()?;
        let editor = workspace.read(cx).active_item_as::<Editor>(cx)?;
        let abs_path = editor_abs_path(&editor, cx)?;
        let editor_ref = editor.read(cx);
        let multi_buffer = editor_ref.buffer().read(cx);
        let snapshot = multi_buffer.snapshot(cx);
        let anchor_selection = editor_ref.selections.newest_anchor();
        let start = anchor_selection.start.to_point(&snapshot);
        let end = anchor_selection.end.to_point(&snapshot);
        let text = if start == end {
            None
        } else {
            Some(SharedString::from(
                snapshot.text_for_range(start..end).collect::<String>(),
            ))
        };
        Some(EditorSelection {
            path: Arc::from(abs_path),
            start,
            end,
            text,
        })
    }

    fn open_file(&self, path: Arc<Path>, focus: bool, cx: &mut App) -> Task<Result<()>> {
        let workspace = self.workspace.clone();
        let window = self.window;
        cx.spawn(async move |cx| {
            let task = cx
                .update_window(window, |_, window, cx| {
                    workspace.update(cx, |workspace, cx| {
                        workspace.open_abs_path(
                            path.to_path_buf(),
                            OpenOptions {
                                focus: Some(focus),
                                ..OpenOptions::default()
                            },
                            window,
                            cx,
                        )
                    })
                })
                .context("open_file: failed to enter window")?
                .context("open_file: workspace dropped")?;
            task.await?;
            Ok(())
        })
    }

    fn save_document(&self, path: Arc<Path>, cx: &mut App) -> Task<Result<()>> {
        let Some(buffer) = self.buffer_for_abs_path(&path, cx) else {
            return Task::ready(Ok(()));
        };
        let Some(workspace) = self.workspace.upgrade() else {
            return Task::ready(Err(anyhow!("workspace dropped")));
        };
        let project = workspace.read(cx).project().clone();
        project.update(cx, |project, cx| project.save_buffer(buffer, cx))
    }

    fn check_dirty(&self, path: Arc<Path>, cx: &App) -> bool {
        self.buffer_for_abs_path(&path, cx)
            .map(|buffer| buffer.read(cx).is_dirty())
            .unwrap_or(false)
    }

    fn get_diagnostics(&self, _path: Option<Arc<Path>>, _cx: &App) -> Vec<DiagnosticInfo> {
        // Diagnostics surfacing is deferred — Claude `/ide` getDiagnostics can ship
        // returning an empty list without breaking the protocol. Wiring this up
        // requires walking the LSP store's per-buffer diagnostic sets and
        // converting `language::Diagnostic` entries; do that when a user asks for it.
        Vec::new()
    }

    fn open_diff_for_review(
        &self,
        _path: Arc<Path>,
        _old_text: String,
        _new_text: String,
        _cx: &mut App,
    ) -> Task<Result<DiffDecision>> {
        // Awaits the Accept/Reject UI in task #3.
        todo!("open_diff_for_review awaits diff Accept/Reject UI")
    }

    fn observe_selection(&self, _callback: SelectionCallback, _cx: &mut App) -> Subscription {
        // Re-subscribing across active-editor focus changes requires interior
        // mutability shared between two observers (workspace active-item change +
        // active editor's selection change). Defer to a follow-up; the Claude
        // connector can fall back to polling `current_selection` until this lands.
        todo!("observe_selection awaits a focus-change-aware re-subscription helper")
    }
}
