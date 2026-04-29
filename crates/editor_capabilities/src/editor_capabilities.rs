use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result, anyhow};
use editor::{Editor, EditorEvent, ToPoint as _};
use gpui::{
    AnyWindowHandle, App, AppContext as _, Entity, SharedString, Subscription, Task, WeakEntity,
};
use language::{Buffer, Point};
use parking_lot::Mutex;
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

/// The active editor in the workspace's center pane group, if any. Skips
/// terminal panels and other non-editor focused items, since AI agents
/// running in side docks should still be able to inspect the user's code
/// editor selection.
fn active_center_editor(workspace: &Workspace, cx: &App) -> Option<Entity<Editor>> {
    workspace.active_pane().read(cx).active_item()?.act_as::<Editor>(cx)
}

/// Fallback for `current_selection` and friends: walk every pane in the
/// workspace's center group and return the first editor we find. Used when
/// the active center pane isn't an editor (e.g. it's a placeholder), so the
/// agent can at least see *some* editor's selection rather than none.
fn any_center_editor(workspace: &Workspace, cx: &App) -> Option<Entity<Editor>> {
    workspace
        .panes()
        .iter()
        .find_map(|pane| pane.read(cx).active_item()?.act_as::<Editor>(cx))
        .or_else(|| {
            workspace.panes().iter().find_map(|pane| {
                pane.read(cx)
                    .items()
                    .find_map(|item| item.act_as::<Editor>(cx))
            })
        })
}

fn editor_abs_path(editor: &Entity<Editor>, cx: &App) -> Option<PathBuf> {
    let multi_buffer = editor.read(cx).buffer();
    let buffer = multi_buffer.read(cx).as_singleton()?;
    let file = buffer.read(cx).file()?;
    let local = file.as_local()?;
    Some(local.abs_path(cx))
}

fn selection_from_editor(editor: &Entity<Editor>, cx: &App) -> Option<EditorSelection> {
    let abs_path = editor_abs_path(editor, cx)?;
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
            log::warn!("Claude /ide getOpenEditors: workspace dropped");
            return Vec::new();
        };
        let workspace = workspace.read(cx);
        let pane_count = workspace.panes().len();
        let active_editor = active_center_editor(workspace, cx);
        let active_path = active_editor
            .as_ref()
            .and_then(|editor| editor_abs_path(editor, cx));

        let mut result = Vec::new();
        let mut item_count = 0usize;
        let mut editor_item_count = 0usize;
        for pane in workspace.panes() {
            let pane = pane.read(cx);
            for item in pane.items() {
                item_count += 1;
                let Some(editor) = item.act_as::<Editor>(cx) else {
                    continue;
                };
                editor_item_count += 1;
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
        log::info!(
            "Claude /ide getOpenEditors: panes={pane_count} items={item_count} editor_items={editor_item_count} returning={}",
            result.len()
        );
        result
    }

    fn current_selection(&self, cx: &App) -> Option<EditorSelection> {
        let Some(workspace) = self.workspace.upgrade() else {
            log::warn!("Claude /ide getCurrentSelection: workspace dropped");
            return None;
        };
        let workspace_ref = workspace.read(cx);
        let pane_count = workspace_ref.panes().len();
        log::info!("Claude /ide getCurrentSelection: scanning workspace ({pane_count} center panes)");
        let editor = match active_center_editor(workspace_ref, cx)
            .or_else(|| any_center_editor(workspace_ref, cx))
        {
            Some(editor) => editor,
            None => {
                log::warn!("Claude /ide getCurrentSelection: no editor found in any center pane");
                return None;
            }
        };
        let selection = selection_from_editor(&editor, cx)?;
        log::info!(
            "Claude /ide getCurrentSelection: path={} start={:?} end={:?} has_text={}",
            selection.path.display(),
            selection.start,
            selection.end,
            selection.text.is_some(),
        );
        Some(selection)
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

    fn observe_selection(&self, callback: SelectionCallback, cx: &mut App) -> Subscription {
        log::info!("Claude /ide observe_selection: subscribing");
        // Tracks the editor whose SelectionsChanged events we currently mirror,
        // along with the live `App::subscribe` handle. Wrapped in a Mutex so the
        // workspace observer below can swap the editor subscription whenever
        // the active center pane flips. The Subscription returned to the caller
        // owns this state; dropping the Subscription clears it (which also
        // drops the editor sub).
        struct State {
            editor: Option<Entity<Editor>>,
            editor_subscription: Option<Subscription>,
        }
        let state = Arc::new(Mutex::new(State {
            editor: None,
            editor_subscription: None,
        }));
        let callback = Arc::new(callback);

        let workspace = self.workspace.clone();
        let resubscribe = {
            let state = state.clone();
            let callback = callback.clone();
            let workspace = workspace.clone();
            Arc::new(move |cx: &mut App| {
                let new_editor = workspace
                    .upgrade()
                    .and_then(|workspace_entity| {
                        let workspace_ref = workspace_entity.read(cx);
                        active_center_editor(workspace_ref, cx)
                            .or_else(|| any_center_editor(workspace_ref, cx))
                    });

                let mut guard = state.lock();
                let same_editor = match (guard.editor.as_ref(), new_editor.as_ref()) {
                    (Some(current), Some(next)) => current.entity_id() == next.entity_id(),
                    (None, None) => true,
                    _ => false,
                };
                if same_editor {
                    return;
                }
                guard.editor_subscription = None;
                guard.editor = new_editor.clone();
                drop(guard);

                if let Some(editor) = new_editor.as_ref() {
                    log::info!(
                        "Claude /ide observe_selection: targeting editor {:?}",
                        editor_abs_path(editor, cx)
                    );
                    let editor_subscription = cx.subscribe(editor, {
                        let callback = callback.clone();
                        move |editor, event: &EditorEvent, cx| {
                            if matches!(event, EditorEvent::SelectionsChanged { .. }) {
                                log::info!("Claude /ide observe_selection: SelectionsChanged fired");
                                let selection = selection_from_editor(&editor, cx);
                                callback(selection, cx);
                            }
                        }
                    });
                    state.lock().editor_subscription = Some(editor_subscription);
                    let selection = selection_from_editor(editor, cx);
                    callback(selection, cx);
                } else {
                    log::info!("Claude /ide observe_selection: no editor to target; pushing null");
                    callback(None, cx);
                }
            })
        };

        // Subscribe to workspace-level active-item flips so we re-target the
        // editor we're listening to. Without this, selection updates from a
        // newly-focused editor would never reach Claude.
        let workspace_subscription = if let Some(workspace_entity) = workspace.upgrade() {
            let resubscribe = resubscribe.clone();
            cx.subscribe(
                &workspace_entity,
                move |_workspace, event: &workspace::Event, cx| {
                    if matches!(event, workspace::Event::ActiveItemChanged) {
                        resubscribe(cx);
                    }
                },
            )
        } else {
            return Subscription::new(|| {});
        };

        // Initial subscribe — pick up the editor that's already active.
        resubscribe(cx);

        let state_holder = state;
        Subscription::join(
            workspace_subscription,
            Subscription::new(move || {
                // Drop the editor subscription when the outer subscription is dropped.
                state_holder.lock().editor_subscription = None;
            }),
        )
    }

}
