use std::any::{Any, TypeId};
use std::path::Path;
use std::sync::Arc;

use anyhow::{Result, anyhow};
use buffer_diff::BufferDiff;
use editor::{Editor, EditorEvent, MultiBuffer};
use futures::channel::oneshot;
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    IntoElement, ParentElement, Render, SharedString, Styled, Subscription, Task, WeakEntity,
    Window, actions, prelude::*,
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

use crate::DiffDecision;

actions!(
    claude_diff,
    [
        /// Accept Claude's proposed edits in the open diff review pane.
        Accept,
        /// Reject Claude's proposed edits in the open diff review pane.
        Reject,
    ]
);

/// Holds the oneshot sender. Lives in an `Arc<Mutex<>>` so the pane's `Drop`
/// path can fire `Cancelled` if neither button was clicked, while normal
/// Accept/Reject paths take the sender out first.
type DecisionSlot = Arc<Mutex<Option<oneshot::Sender<DiffDecision>>>>;

pub(crate) struct ClaudeDiffPane {
    working_buffer: Entity<Buffer>,
    editor: Entity<Editor>,
    decision: DecisionSlot,
    title: SharedString,
    focus_handle: FocusHandle,
    _multibuffer: Entity<MultiBuffer>,
    _diff: Entity<BufferDiff>,
    _subscriptions: Vec<Subscription>,
    _cancel_on_drop: CancelOnDrop,
}

struct CancelOnDrop {
    decision: DecisionSlot,
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if let Some(sender) = self.decision.lock().take() {
            let _ = sender.send(DiffDecision::Cancelled);
        }
    }
}

impl ClaudeDiffPane {
    pub(crate) fn new(
        _path: Arc<Path>,
        title: SharedString,
        old_text: String,
        new_text: String,
        _workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut App,
    ) -> (Entity<Self>, oneshot::Receiver<DiffDecision>) {
        let (sender, receiver) = oneshot::channel();
        let decision: DecisionSlot = Arc::new(Mutex::new(Some(sender)));

        // v1 builds a detached buffer without language detection; the diff
        // pane renders without syntax highlighting. Async language load is
        // a follow-up.
        let working_buffer = cx.new(|cx| Buffer::local(new_text, cx));

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
        let pane = cx.new(|cx| {
            let editor = cx.new(|cx| {
                let mut editor =
                    Editor::for_multibuffer(multibuffer_for_editor.clone(), None, window, cx);
                editor.set_expand_all_diff_hunks(cx);
                editor
            });
            let focus_handle = cx.focus_handle();
            ClaudeDiffPane {
                working_buffer,
                editor,
                decision: decision.clone(),
                title,
                focus_handle,
                _multibuffer: multibuffer,
                _diff: diff_entity,
                _subscriptions: Vec::new(),
                _cancel_on_drop: CancelOnDrop {
                    decision: decision.clone(),
                },
            }
        });

        (pane, receiver)
    }

    fn accept(&mut self, _: &Accept, _window: &mut Window, cx: &mut Context<Self>) {
        let final_text = self.working_buffer.read(cx).text();
        self.resolve(cx, DiffDecision::Accept { final_text });
    }

    fn reject(&mut self, _: &Reject, _window: &mut Window, cx: &mut Context<Self>) {
        self.resolve(cx, DiffDecision::Reject);
    }

    fn resolve(&mut self, cx: &mut Context<Self>, decision: DiffDecision) {
        let sender = self.decision.lock().take();
        if let Some(sender) = sender {
            let _ = sender.send(decision);
            cx.emit(ItemEvent::CloseItem);
        }
    }
}

impl EventEmitter<ItemEvent> for ClaudeDiffPane {}
impl EventEmitter<EditorEvent> for ClaudeDiffPane {}

impl Focusable for ClaudeDiffPane {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.editor.focus_handle(cx)
    }
}

impl Render for ClaudeDiffPane {
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
            .child(Label::new(format!("Claude diff: {}", self.title)))
            .child(gpui::div().flex_1())
            .child(
                Button::new("claude-diff-reject", "Reject")
                    .style(ButtonStyle::Subtle)
                    .on_click(cx.listener(|this, _event, window, cx| {
                        this.reject(&Reject, window, cx);
                    })),
            )
            .child(
                Button::new("claude-diff-accept", "Accept")
                    .style(ButtonStyle::Filled)
                    .on_click(cx.listener(|this, _event, window, cx| {
                        this.accept(&Accept, window, cx);
                    })),
            );

        gpui::div()
            .flex()
            .flex_col()
            .size_full()
            .key_context("ClaudeDiff")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::accept))
            .on_action(cx.listener(Self::reject))
            .child(header)
            .child(self.editor.clone())
    }
}

impl Item for ClaudeDiffPane {
    type Event = ItemEvent;

    fn tab_icon(&self, _window: &Window, _cx: &App) -> Option<Icon> {
        Some(Icon::new(IconName::ZedAssistant).color(Color::Muted))
    }

    fn tab_content(&self, params: TabContentParams, _window: &Window, _cx: &App) -> AnyElement {
        Label::new(format!("Claude diff: {}", self.title))
            .color(if params.selected {
                Color::Default
            } else {
                Color::Muted
            })
            .into_any_element()
    }

    fn tab_content_text(&self, _detail: usize, _cx: &App) -> SharedString {
        SharedString::from(format!("Claude diff: {}", self.title))
    }

    fn tab_tooltip_text(&self, _: &App) -> Option<SharedString> {
        Some("Review Claude's proposed edits".into())
    }

    fn telemetry_event_text(&self) -> Option<&'static str> {
        Some("Claude /ide diff opened")
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
                let (pane, receiver) = ClaudeDiffPane::new(
                    path.clone(),
                    title.clone(),
                    old_text,
                    new_text,
                    workspace.clone(),
                    window,
                    cx,
                );
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
            .map_err(|err| anyhow!("Claude /ide openDiff: failed to enter window: {err:#}"))??;
        receiver
            .await
            .map_err(|_| anyhow!("Claude /ide openDiff pane resolved without a decision"))
    })
}
