//! A markdown editor with a preview/source toggle.
//!
//! `.md` files opened from the context panel open here. By default they show a
//! rendered, read-only markdown preview (headings, code with syntax
//! highlighting, tables, links, task lists, mermaid diagrams). A centered
//! toolbar toggle flips to a plain-text editor over the same file buffer to edit
//! the raw markdown, and back to preview. The file `Buffer` is the source of
//! truth, so saving works as usual. Files outside the project worktree (e.g. the
//! global `~/.claude/CLAUDE.md`) are supported via a local buffer.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use editor::Editor;
use gpui::{
    App, Context, EdgesRefinement, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement, IntoElement, Length, ParentElement, Pixels, Render, ScrollHandle,
    SharedString, StatefulInteractiveElement, StyleRefinement, Subscription, Task, Window, px,
};
use language::{Buffer, BufferEvent, LanguageRegistry};
use markdown::{Markdown, MarkdownElement, MarkdownFont, MarkdownOptions, MarkdownStyle};
use project::Project;
use ui::prelude::*;
use ui::{Icon, IconName};
use workspace::Workspace;
use workspace::item::{Item, ItemBufferKind, ItemEvent, SaveOptions};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Preview,
    Edit,
}

pub struct MarkdownEditor {
    buffer: Entity<Buffer>,
    abs_path: PathBuf,
    mode: Mode,
    markdown: Entity<Markdown>,
    editor: Entity<Editor>,
    focus_handle: FocusHandle,
    scroll_handle: ScrollHandle,
    _subscriptions: Vec<Subscription>,
}

impl MarkdownEditor {
    pub fn new(
        buffer: Entity<Buffer>,
        project: Entity<Project>,
        abs_path: PathBuf,
        language_registry: Arc<LanguageRegistry>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let editor = cx.new(|cx| Editor::for_buffer(buffer.clone(), Some(project), window, cx));

        let markdown = cx.new({
            let buffer = buffer.clone();
            move |cx| {
                Markdown::new_with_options(
                    buffer.read(cx).text().into(),
                    Some(language_registry),
                    None,
                    MarkdownOptions {
                        parse_html: true,
                        render_mermaid_diagrams: true,
                        parse_heading_slugs: true,
                        ..Default::default()
                    },
                    cx,
                )
            }
        });

        let subscription = cx.subscribe(&buffer, |this, _buffer, event: &BufferEvent, cx| {
            match event {
                // Keep the preview in sync with external/reload edits while it's
                // showing; edits made in the source editor are re-parsed when we
                // toggle back to preview.
                BufferEvent::Edited { .. } | BufferEvent::Reloaded => {
                    if this.mode == Mode::Preview {
                        this.refresh_preview(cx);
                        cx.notify();
                    }
                    cx.emit(());
                }
                BufferEvent::DirtyChanged | BufferEvent::Saved => cx.emit(()),
                _ => {}
            }
        });

        Self {
            buffer,
            abs_path,
            mode: Mode::Preview,
            markdown,
            editor,
            focus_handle: cx.focus_handle(),
            scroll_handle: ScrollHandle::new(),
            _subscriptions: vec![subscription],
        }
    }

    /// Re-parse the latest buffer contents into the preview's markdown.
    fn refresh_preview(&self, cx: &mut Context<Self>) {
        let text = self.buffer.read(cx).text();
        self.markdown.update(cx, |markdown, cx| {
            markdown.reset(text.into(), cx);
        });
    }

    fn set_mode(&mut self, mode: Mode, window: &mut Window, cx: &mut Context<Self>) {
        if self.mode == mode {
            return;
        }
        self.mode = mode;
        match mode {
            Mode::Edit => window.focus(&self.editor.focus_handle(cx), cx),
            Mode::Preview => {
                self.refresh_preview(cx);
                window.focus(&self.focus_handle, cx);
            }
        }
        cx.notify();
    }

    /// A centered, accent-highlighted segmented control: Preview | Edit.
    fn render_toggle(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        let accent = colors.text_accent;
        let active_bg = accent.opacity(0.15);
        let muted = colors.text_muted;
        let preview_active = self.mode == Mode::Preview;

        let segment = |id: &'static str, label: &'static str, active: bool| {
            div()
                .id(id)
                .px_3()
                .py_0p5()
                .rounded_md()
                .cursor_pointer()
                .when(active, |el| el.bg(active_bg).text_color(accent))
                .when(!active, |el| el.text_color(muted))
                .child(label)
        };

        h_flex()
            .p_0p5()
            .gap_0p5()
            .rounded_lg()
            .border_1()
            .border_color(colors.border)
            .bg(colors.element_background)
            .child(
                segment("markdown-toggle-preview", "Preview", preview_active).on_click(
                    cx.listener(|this, _event, window, cx| {
                        this.set_mode(Mode::Preview, window, cx)
                    }),
                ),
            )
            .child(
                segment("markdown-toggle-edit", "Edit", !preview_active).on_click(
                    cx.listener(|this, _event, window, cx| this.set_mode(Mode::Edit, window, cx)),
                ),
            )
    }
}

/// A `StyleRefinement` setting only top/bottom margins, for spacing markdown
/// blocks apart in the preview.
fn block_margins(top: Pixels, bottom: Pixels) -> StyleRefinement {
    StyleRefinement {
        margin: EdgesRefinement {
            top: Some(Length::Definite(top.into())),
            bottom: Some(Length::Definite(bottom.into())),
            left: None,
            right: None,
        },
        ..Default::default()
    }
}

impl Focusable for MarkdownEditor {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        // In edit mode, hand focus to the inner editor so keystrokes reach it.
        match self.mode {
            Mode::Edit => self.editor.focus_handle(cx),
            Mode::Preview => self.focus_handle.clone(),
        }
    }
}

impl EventEmitter<()> for MarkdownEditor {}

impl Item for MarkdownEditor {
    type Event = ();

    fn tab_icon(&self, _window: &Window, _cx: &App) -> Option<Icon> {
        Some(Icon::new(IconName::FileDoc))
    }

    fn tab_content_text(&self, _detail: usize, _cx: &App) -> SharedString {
        self.abs_path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned().into())
            .unwrap_or_else(|| SharedString::from("untitled.md"))
    }

    fn telemetry_event_text(&self) -> Option<&'static str> {
        Some("Markdown Editor Opened")
    }

    fn to_item_events(_event: &Self::Event, f: &mut dyn FnMut(ItemEvent)) {
        f(ItemEvent::UpdateTab);
    }

    fn buffer_kind(&self, _cx: &App) -> ItemBufferKind {
        ItemBufferKind::Singleton
    }

    fn is_dirty(&self, cx: &App) -> bool {
        self.buffer.read(cx).is_dirty()
    }

    fn has_conflict(&self, cx: &App) -> bool {
        self.buffer.read(cx).has_conflict()
    }

    fn can_save(&self, _cx: &App) -> bool {
        true
    }

    fn save(
        &mut self,
        _options: SaveOptions,
        project: Entity<Project>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        let buffer = self.buffer.clone();
        cx.spawn(async move |_this, cx| {
            project
                .update(cx, |project, cx| project.save_buffer(buffer, cx))
                .await
        })
    }
}

impl Render for MarkdownEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let header = h_flex()
            .flex_none()
            .w_full()
            .px_2()
            .py_1p5()
            .justify_center()
            .border_b_1()
            .border_color(cx.theme().colors().border)
            .bg(cx.theme().colors().editor_background)
            .child(self.render_toggle(cx));

        let body = match self.mode {
            Mode::Preview => {
                let mut style = MarkdownStyle::themed(MarkdownFont::Editor, window, cx);
                style.heading = block_margins(px(24.), px(10.));
                style.paragraph = block_margins(px(0.), px(14.));

                div()
                    .id("markdown-preview")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll_handle)
                    .bg(cx.theme().colors().editor_background)
                    .px_8()
                    .py_6()
                    .child(
                        MarkdownElement::new(self.markdown.clone(), style)
                            .on_url_click(|url, _window, cx| cx.open_url(&url)),
                    )
                    .into_any_element()
            }
            Mode::Edit => self.editor.clone().into_any_element(),
        };

        v_flex()
            .size_full()
            .track_focus(&self.focus_handle)
            .child(header)
            .child(div().flex_1().min_h(px(0.)).child(body))
    }
}

/// Open `abs_path` as a [`MarkdownEditor`] in the workspace's active pane,
/// activating an existing tab for the same path if one is already open. Works
/// for files both inside and outside the project worktree.
pub fn open_markdown_in_editor(
    workspace: &mut Workspace,
    abs_path: &Path,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let project = workspace.project().clone();
    let abs_path = abs_path.to_path_buf();

    // Reactivate an already-open markdown editor for this path.
    let pane = workspace.active_pane().clone();
    let existing_index = {
        let pane = pane.read(cx);
        pane.items_of_type::<MarkdownEditor>()
            .find(|item| item.read(cx).abs_path == abs_path)
            .and_then(|item| pane.index_for_item(&item))
    };
    if let Some(index) = existing_index {
        pane.update(cx, |pane, cx| {
            pane.activate_item(index, true, true, window, cx);
        });
        return;
    }

    let language_registry = project.read(cx).languages().clone();
    let buffer_task = match project
        .read(cx)
        .project_path_for_absolute_path(&abs_path, cx)
    {
        Some(project_path) => {
            project.update(cx, |project, cx| project.open_buffer(project_path, cx))
        }
        None => project.update(cx, |project, cx| {
            project.open_local_buffer(abs_path.clone(), cx)
        }),
    };

    cx.spawn_in(window, async move |workspace, cx| {
        let buffer = buffer_task.await?;
        workspace.update_in(cx, |workspace, window, cx| {
            let editor = cx.new(|cx| {
                MarkdownEditor::new(
                    buffer,
                    project.clone(),
                    abs_path.clone(),
                    language_registry.clone(),
                    window,
                    cx,
                )
            });
            workspace.active_pane().update(cx, |pane, cx| {
                pane.add_item(Box::new(editor), true, true, None, window, cx);
            });
        })?;
        anyhow::Ok(())
    })
    .detach_and_log_err(cx);
}

/// No-op today; reserved for registering actions (e.g. a keybinding for the
/// preview/edit toggle) in a later change. Called from the binary's startup so
/// the wiring is in place.
pub fn init(_cx: &mut App) {}
