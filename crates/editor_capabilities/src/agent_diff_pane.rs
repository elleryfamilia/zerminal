use std::any::{Any, TypeId};
use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use anyhow::{Result, anyhow};
use buffer_diff::BufferDiff;
use editor::{Editor, EditorEvent, MultiBuffer};
use futures::channel::oneshot;
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, EntityId, EventEmitter, FocusHandle,
    Focusable, IntoElement, ParentElement, Render, SharedString, Styled, Task, WeakEntity, Window,
    actions, prelude::*,
};
use language::{Buffer, LineEnding};
use parking_lot::Mutex;
use theme::ActiveTheme as _;
use ui::{
    Button, ButtonCommon, ButtonStyle, Clickable, Color, Icon, IconName, Label, LabelCommon as _,
};
use workspace::{
    Item, ItemNavHistory, Workspace,
    item::{ItemEvent, TabContentParams},
};

use crate::{DiffDecision, OpenDiffRegistry};

actions!(
    agent_diff,
    [
        /// Accept the agent's proposed edits in the open diff review pane.
        Accept,
        /// Reject the agent's proposed edits in the open diff review pane.
        Reject,
    ]
);

/// Holds the oneshot sender. Lives in an `Arc<Mutex<>>` so the pane's `Drop`
/// path can fire `Cancelled` if neither button was clicked, while normal
/// Accept/Reject paths take the sender out first.
type DecisionSlot = Arc<Mutex<Option<oneshot::Sender<DiffDecision>>>>;

/// Carries the pane's own [`EntityId`] so a stale pane (orphaned by a
/// `tab_name` collision under last-write-wins) cannot evict a newer
/// entry: see [`remove_if_entity_matches`].
#[derive(Clone)]
struct DeregisterRequest {
    tab_name: String,
    entity_id: EntityId,
    registry: OpenDiffRegistry,
}

/// Foreground-only slot; `DeregisterRequest` carries an
/// `Rc<RefCell<...>>` registry handle so `Send`/`Sync` would be a lie.
/// `Rc<RefCell<...>>` instead of `Arc<Mutex<...>>` — clippy's
/// `arc_with_non_send_sync` lint requires this.
type DeregisterSlot = Rc<RefCell<Option<DeregisterRequest>>>;

pub(crate) struct AgentDiffPane {
    working_buffer: Entity<Buffer>,
    editor: Entity<Editor>,
    decision: DecisionSlot,
    deregister: DeregisterSlot,
    title: SharedString,
    focus_handle: FocusHandle,
    _multibuffer: Entity<MultiBuffer>,
    _diff: Entity<BufferDiff>,
    // `decision` and `deregister` are dropped before `_cancel_on_drop`,
    // but the guard holds independent `Arc` / `Rc` clones of both slots
    // so the data lives until the last clone goes. Reordering would
    // remain sound for the same reason — declaration order is not
    // load-bearing here, only that the slots are reference-counted.
    _cancel_on_drop: CancelAndDeregisterOnDrop,
}

/// Fires `Cancelled` and removes the pane's registry entry when the
/// pane is dropped without anyone (user button, `resolve`,
/// `close_from_model`) having taken the slots first — i.e. the user
/// closed the tab manually. Both slots are `Option`s; a winning path
/// takes its slot out and Drop becomes a no-op for that slot.
struct CancelAndDeregisterOnDrop {
    decision: DecisionSlot,
    deregister: DeregisterSlot,
}

impl Drop for CancelAndDeregisterOnDrop {
    fn drop(&mut self) {
        if let Some(sender) = self.decision.lock().take() {
            let _ = sender.send(DiffDecision::Cancelled);
        }
        if let Some(request) = self.deregister.borrow_mut().take() {
            remove_if_entity_matches(&request);
        }
    }
}

/// Verify the registry still points at our pane before evicting. Under
/// last-write-wins, two terminals opening diffs with the same `tab_name`
/// produce one "winning" entry; we must not let the loser's deregister
/// path remove the winner's entry. The EntityId comparison is the
/// guard. Mirrors the identical logic in `CopilotTerminalRouter::unregister`.
fn remove_if_entity_matches(request: &DeregisterRequest) {
    let mut map = request.registry.borrow_mut();
    if let Some(weak) = map.get(&request.tab_name)
        && weak.entity_id() == request.entity_id
    {
        map.remove(&request.tab_name);
    }
}

impl AgentDiffPane {
    pub(crate) fn new(
        path: Arc<Path>,
        title: SharedString,
        old_text: String,
        new_text: String,
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut App,
    ) -> (Entity<Self>, oneshot::Receiver<DiffDecision>) {
        let (sender, receiver) = oneshot::channel();
        let decision: DecisionSlot = Arc::new(Mutex::new(Some(sender)));

        let working_buffer = cx.new(|cx| Buffer::local(new_text, cx));
        // Async language load so the diff renders with syntax highlighting.
        // The buffer is detached (not owned by Project), so we go through
        // the workspace's LanguageRegistry directly. Failures are logged
        // and silently fall back to plaintext — the diff is still readable
        // without colors.
        if let Some(workspace_entity) = workspace.upgrade() {
            let languages = workspace_entity.read(cx).project().read(cx).languages().clone();
            let buffer_handle = working_buffer.downgrade();
            cx.spawn(async move |cx| {
                let language = languages.load_language_for_file_path(&path).await;
                match language {
                    Ok(language) => {
                        let _ = buffer_handle.update(cx, |buffer, cx| {
                            buffer.set_language(Some(language), cx);
                        });
                    }
                    Err(error) => {
                        log::debug!(
                            "AgentDiffPane: no language for path {}: {error:#}",
                            path.display()
                        );
                    }
                }
            })
            .detach();
        }

        let snapshot = working_buffer.read(cx).text_snapshot();
        let diff_entity = cx.new(|cx| BufferDiff::new(&snapshot, cx));
        diff_entity.update(cx, |diff, cx| {
            let mut base = old_text;
            LineEnding::normalize(&mut base);
            let _ = diff.set_base_text(Some(Arc::from(base)), None, snapshot.clone(), cx);
        });

        let multibuffer = cx.new(|cx| {
            let mut mb = MultiBuffer::singleton(working_buffer.clone(), cx);
            mb.add_diff(diff_entity.clone(), cx);
            mb
        });

        let multibuffer_for_editor = multibuffer.clone();
        let deregister: DeregisterSlot = Rc::new(RefCell::new(None));
        let pane = cx.new(|cx| {
            let editor = cx.new(|cx| {
                let mut editor =
                    Editor::for_multibuffer(multibuffer_for_editor.clone(), None, window, cx);
                editor.set_expand_all_diff_hunks(cx);
                editor
            });
            let focus_handle = cx.focus_handle();
            AgentDiffPane {
                working_buffer,
                editor,
                decision: decision.clone(),
                deregister: deregister.clone(),
                title,
                focus_handle,
                _multibuffer: multibuffer,
                _diff: diff_entity,
                _cancel_on_drop: CancelAndDeregisterOnDrop {
                    decision: decision.clone(),
                    deregister: deregister.clone(),
                },
            }
        });

        (pane, receiver)
    }

    /// Arm the pane's self-deregister hook. `spawn_diff_review` calls
    /// this once, immediately after construction, when the pane was
    /// opened with a `tab_name`. After arming, `resolve` /
    /// `close_from_model` / Drop all converge on
    /// [`remove_if_entity_matches`] which compares EntityIds before
    /// evicting — protects against last-write-wins races.
    pub(crate) fn set_registry_handle(
        &self,
        tab_name: String,
        entity_id: EntityId,
        registry: OpenDiffRegistry,
    ) {
        *self.deregister.borrow_mut() = Some(DeregisterRequest {
            tab_name,
            entity_id,
            registry,
        });
    }

    fn accept(&mut self, _: &Accept, _window: &mut Window, cx: &mut Context<Self>) {
        let final_text = self.working_buffer.read(cx).text();
        self.resolve(cx, DiffDecision::Accept { final_text });
    }

    fn reject(&mut self, _: &Reject, _window: &mut Window, cx: &mut Context<Self>) {
        self.resolve(cx, DiffDecision::Reject);
    }

    /// Model-initiated programmatic close (Copilot `close_diff` MCP tool).
    /// Sends the pending decision as [`DiffDecision::ClosedByModel`] so
    /// the open_diff response carries the upstream-verified
    /// `closed_via_tool` trigger, and emits `CloseItem`.
    ///
    /// Sender + deregister are taken BEFORE emitting CloseItem so the
    /// subsequent Drop's `CancelAndDeregisterOnDrop` finds both slots
    /// empty and becomes a no-op — mirrors the existing `resolve`
    /// ordering exactly.
    pub(crate) fn close_from_model(&mut self, cx: &mut Context<Self>) {
        let sender = self.decision.lock().take();
        let deregister = self.deregister.borrow_mut().take();
        if let Some(sender) = sender {
            let _ = sender.send(DiffDecision::ClosedByModel);
        }
        if let Some(request) = deregister {
            remove_if_entity_matches(&request);
        }
        // Emit even if the sender was already taken (e.g. user accepted
        // first): if the pane is still on screen, closing it is still
        // the correct UI outcome for a model-initiated close request.
        cx.emit(ItemEvent::CloseItem);
    }

    fn resolve(&mut self, cx: &mut Context<Self>, decision: DiffDecision) {
        let sender = self.decision.lock().take();
        let deregister = self.deregister.borrow_mut().take();
        if let Some(sender) = sender {
            let _ = sender.send(decision);
        }
        if let Some(request) = deregister {
            remove_if_entity_matches(&request);
        }
        // Emit unconditionally — symmetric with `close_from_model`. If
        // the model closed first, `sender` is gone and the receiver
        // already saw `ClosedByModel`, but the tab may still be on
        // screen until the workspace drains the event queue, and the
        // user's button click should reliably finish the close. The
        // workspace's CloseItem handler no-ops on an already-closed
        // item, so emitting twice is safe.
        cx.emit(ItemEvent::CloseItem);
    }
}

impl EventEmitter<ItemEvent> for AgentDiffPane {}
impl EventEmitter<EditorEvent> for AgentDiffPane {}

impl Focusable for AgentDiffPane {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.editor.focus_handle(cx)
    }
}

impl Render for AgentDiffPane {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let header = gpui::div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(cx.theme().colors().border)
            .child(Icon::new(IconName::ZedAssistant).color(Color::Accent))
            .child(Label::new(format!("Agent diff: {}", self.title)))
            .child(gpui::div().flex_1())
            .child(
                Button::new("agent-diff-reject", "Reject")
                    .style(ButtonStyle::Subtle)
                    .on_click(cx.listener(|this, _event, window, cx| {
                        this.reject(&Reject, window, cx);
                    })),
            )
            .child(
                Button::new("agent-diff-accept", "Accept")
                    .style(ButtonStyle::Filled)
                    .on_click(cx.listener(|this, _event, window, cx| {
                        this.accept(&Accept, window, cx);
                    })),
            );

        gpui::div()
            .flex()
            .flex_col()
            .size_full()
            .key_context("AgentDiff")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::accept))
            .on_action(cx.listener(Self::reject))
            .child(header)
            .child(self.editor.clone())
    }
}

impl Item for AgentDiffPane {
    type Event = ItemEvent;

    fn tab_icon(&self, _window: &Window, _cx: &App) -> Option<Icon> {
        Some(Icon::new(IconName::ZedAssistant).color(Color::Muted))
    }

    fn tab_content(&self, params: TabContentParams, _window: &Window, _cx: &App) -> AnyElement {
        Label::new(format!("Agent diff: {}", self.title))
            .color(if params.selected {
                Color::Default
            } else {
                Color::Muted
            })
            .into_any_element()
    }

    fn tab_content_text(&self, _detail: usize, _cx: &App) -> SharedString {
        SharedString::from(format!("Agent diff: {}", self.title))
    }

    fn tab_tooltip_text(&self, _: &App) -> Option<SharedString> {
        Some("Review the agent's proposed edits".into())
    }

    fn telemetry_event_text(&self) -> Option<&'static str> {
        Some("Agent /ide diff opened")
    }

    fn to_item_events(event: &ItemEvent, f: &mut dyn FnMut(ItemEvent)) {
        f(*event);
    }

    fn is_dirty(&self, _: &App) -> bool {
        false
    }

    fn has_conflict(&self, _: &App) -> bool {
        false
    }

    fn can_save(&self, _: &App) -> bool {
        false
    }

    fn act_as_type<'a>(
        &'a self,
        type_id: TypeId,
        self_handle: &'a Entity<Self>,
        _: &'a App,
    ) -> Option<gpui::AnyEntity> {
        if type_id == TypeId::of::<Self>() {
            Some(self_handle.clone().into())
        } else if type_id == TypeId::of::<Editor>() {
            Some(self.editor.clone().into())
        } else {
            None
        }
    }

    fn navigate(
        &mut self,
        data: Arc<dyn Any + Send>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.editor
            .update(cx, |editor, cx| editor.navigate(data, window, cx))
    }

    fn set_nav_history(
        &mut self,
        nav_history: ItemNavHistory,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.editor.update(cx, |editor, _| {
            editor.set_nav_history(Some(nav_history));
        });
    }

    fn deactivated(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.editor
            .update(cx, |editor, cx| editor.deactivated(window, cx));
    }

    fn added_to_workspace(
        &mut self,
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.editor.update(cx, |editor, cx| {
            editor.added_to_workspace(workspace, window, cx)
        });
    }
}

pub(crate) fn spawn_diff_review(
    workspace: WeakEntity<Workspace>,
    window: gpui::AnyWindowHandle,
    tab_name: Option<String>,
    registry: OpenDiffRegistry,
    path: Arc<Path>,
    old_text: String,
    new_text: String,
    cx: &mut App,
) -> Task<Result<DiffDecision>> {
    let title: SharedString = path
        .file_name()
        .map(|name| SharedString::from(name.to_string_lossy().into_owned()))
        .unwrap_or_else(|| SharedString::from(path.to_string_lossy().into_owned()));

    cx.spawn(async move |cx| {
        let receiver = cx
            .update_window(window, |_, window, cx| {
                let workspace_entity = workspace
                    .upgrade()
                    .ok_or_else(|| anyhow!("workspace dropped"))?;
                let (pane, receiver) = AgentDiffPane::new(
                    path.clone(),
                    title.clone(),
                    old_text,
                    new_text,
                    workspace.clone(),
                    window,
                    cx,
                );
                // Register before adding to the pane so a model-side
                // `close_diff` racing with our return path always sees a
                // consistent registry. Last-write-wins on duplicate
                // `tab_name` — see `remove_if_entity_matches` for the
                // EntityId guard that protects against an orphaned
                // pane's deregister evicting the new entry.
                if let Some(tab_name) = tab_name {
                    let entity_id = pane.entity_id();
                    registry
                        .borrow_mut()
                        .insert(tab_name.clone(), pane.downgrade());
                    pane.read(cx)
                        .set_registry_handle(tab_name, entity_id, registry.clone());
                }
                workspace_entity.update(cx, |workspace, cx| {
                    workspace.add_item_to_active_pane(
                        Box::new(pane.clone()),
                        None,
                        true,
                        window,
                        cx,
                    );
                });
                Ok::<_, anyhow::Error>(receiver)
            })
            .map_err(|err| anyhow!("AgentDiffPane: failed to enter window: {err:#}"))??;
        receiver
            .await
            .map_err(|_| anyhow!("AgentDiffPane resolved without a decision"))
    })
}
