//! A "block-at-a-time" markdown editor.
//!
//! Markdown files opened from the context panel render here as a vertical stack
//! of formatted blocks instead of a plain-text editor. The file's `Buffer` stays
//! the source of truth (so saving and the rest of the app observe edits); this
//! view just parses the buffer into top-level markdown blocks and renders each
//! one with the shared `markdown` crate.
//!
//! Milestone 1 is read-only rendering plus the context-panel wiring. Later
//! milestones add click-to-edit (a small `Editor` over the active block's range),
//! a rendered/source toggle, and a formatting toolbar.

use std::ops::Range;
use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use editor::{Editor, EditorMode, MultiBufferOffset, SelectionEffects};
use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement, IntoElement,
    ParentElement, Render, ScrollHandle, SharedString, Styled, Subscription, Task, Window,
};
use language::{Buffer, BufferEvent, LanguageRegistry};
use markdown::{Markdown, MarkdownElement, MarkdownFont, MarkdownStyle};
use multi_buffer::{MultiBuffer, PathKey};
use project::{Project, ProjectPath};
use ui::prelude::*;
use ui::{Icon, IconName};
use workspace::item::{Item, ItemBufferKind, ItemEvent, SaveOptions};
use workspace::{OpenOptions, Workspace};

/// One top-level markdown block: its byte range in the buffer's source and a
/// `Markdown` entity holding that block's substring for rendering.
struct Block {
    range: Range<usize>,
    markdown: Entity<Markdown>,
}

/// The block currently being edited: a small auto-height `Editor` mounted over
/// just that block's range of the shared buffer.
struct ActiveBlock {
    index: usize,
    editor: Entity<Editor>,
    _subscriptions: Vec<Subscription>,
}

pub struct MarkdownEditor {
    buffer: Entity<Buffer>,
    project: Entity<Project>,
    project_path: Option<ProjectPath>,
    title: SharedString,
    language_registry: Arc<LanguageRegistry>,
    focus_handle: FocusHandle,
    scroll_handle: ScrollHandle,
    blocks: Vec<Block>,
    active: Option<ActiveBlock>,
    _subscriptions: Vec<Subscription>,
}

impl MarkdownEditor {
    pub fn new(
        buffer: Entity<Buffer>,
        project: Entity<Project>,
        project_path: Option<ProjectPath>,
        title: SharedString,
        language_registry: Arc<LanguageRegistry>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscription = cx.subscribe(&buffer, |this, _buffer, event: &BufferEvent, cx| {
            match event {
                // While a block is being edited inline, the inline editor already
                // shows live source; skip rebuilding so block boundaries don't churn
                // mid-edit. The tab's dirty state may have changed either way.
                BufferEvent::Edited { .. } | BufferEvent::Reloaded => {
                    if this.active.is_none() {
                        this.rebuild_blocks(cx);
                        cx.notify();
                    }
                    cx.emit(());
                }
                BufferEvent::DirtyChanged | BufferEvent::Saved => cx.emit(()),
                _ => {}
            }
        });

        let mut this = Self {
            buffer,
            project,
            project_path,
            title,
            language_registry,
            focus_handle: cx.focus_handle(),
            scroll_handle: ScrollHandle::new(),
            blocks: Vec::new(),
            active: None,
            _subscriptions: vec![subscription],
        };
        this.rebuild_blocks(cx);
        this
    }

    /// Re-parse the buffer into top-level blocks and build a `Markdown` entity
    /// for each. Called on construction and whenever the buffer changes.
    fn rebuild_blocks(&mut self, cx: &mut Context<Self>) {
        let source = self.buffer.read(cx).text();
        let language_registry = self.language_registry.clone();
        self.blocks = split_root_blocks(&source)
            .into_iter()
            .map(|range| {
                let substring: SharedString = source[range.clone()].to_string().into();
                let markdown = cx.new(|cx| {
                    Markdown::new(substring, Some(language_registry.clone()), None, cx)
                });
                Block { range, markdown }
            })
            .collect();
    }

    /// Mount an inline editor over `block_index`'s range and focus it at
    /// `offset_within_block` (a byte offset into that block's source). The editor
    /// edits the shared buffer through a single-excerpt multibuffer, so changes
    /// are saved like any other edit.
    fn enter_edit(
        &mut self,
        block_index: usize,
        offset_within_block: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(block) = self.blocks.get(block_index) else {
            return;
        };
        let range = block.range.clone();
        let buffer = self.buffer.clone();
        let snapshot = buffer.read(cx).snapshot();
        let start = snapshot.offset_to_point(range.start);
        let end = snapshot.offset_to_point(range.end);
        let capability = buffer.read(cx).capability();

        let multibuffer = cx.new(|cx| {
            let mut multibuffer = MultiBuffer::new(capability);
            multibuffer.set_excerpts_for_path(
                PathKey::sorted(0),
                buffer.clone(),
                [start..end],
                0,
                cx,
            );
            multibuffer
        });

        let project = self.project.clone();
        let editor = cx.new(|cx| {
            let mut editor = Editor::new(
                EditorMode::AutoHeight {
                    min_lines: 1,
                    max_lines: None,
                },
                multibuffer,
                Some(project),
                window,
                cx,
            );
            editor.set_show_gutter(false, cx);
            editor.set_show_line_numbers(false, cx);
            editor.set_show_indent_guides(false, cx);
            editor.change_selections(SelectionEffects::no_scroll(), window, cx, |selections| {
                selections.select_ranges([
                    MultiBufferOffset(offset_within_block)..MultiBufferOffset(offset_within_block),
                ]);
            });
            editor
        });

        let editor_id = editor.entity_id();
        let focus_handle = editor.focus_handle(cx);
        let blur_subscription = cx.on_blur(&focus_handle, window, move |_this, window, cx| {
            // Defer so a click that activates a different block can swap `active`
            // before we decide this editor lost focus for good (block-to-block
            // handoff).
            cx.defer_in(window, move |this, _window, cx| {
                if this
                    .active
                    .as_ref()
                    .is_some_and(|active| active.editor.entity_id() == editor_id)
                {
                    this.active = None;
                    this.rebuild_blocks(cx);
                    cx.notify();
                }
            });
        });
        window.focus(&focus_handle, cx);

        self.active = Some(ActiveBlock {
            index: block_index,
            editor,
            _subscriptions: vec![blur_subscription],
        });
        cx.notify();
    }
}

/// Split markdown `source` into the byte ranges of its top-level (root) blocks.
///
/// We track nesting depth over `pulldown-cmark`'s offset-carrying event stream
/// and record each range that opens at depth 0. This mirrors how the `markdown`
/// crate computes its root-block starts (the parsed-markdown accessor there is
/// test-only, so we parse boundaries ourselves).
fn split_root_blocks(source: &str) -> Vec<Range<usize>> {
    use pulldown_cmark::{Event, Options, Parser};

    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_FOOTNOTES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);

    let mut blocks: Vec<Range<usize>> = Vec::new();
    let mut depth: i32 = 0;
    let mut current_start: Option<usize> = None;

    for (event, range) in Parser::new_ext(source, options).into_offset_iter() {
        match event {
            Event::Start(_) => {
                if depth == 0 {
                    current_start = Some(range.start);
                }
                depth += 1;
            }
            Event::End(_) => {
                depth -= 1;
                if depth == 0
                    && let Some(start) = current_start.take()
                {
                    blocks.push(start..range.end);
                }
            }
            // Standalone block-level events that aren't wrapped in Start/End.
            Event::Rule | Event::Html(_) => {
                if depth == 0 {
                    blocks.push(range);
                }
            }
            _ => {}
        }
    }

    // Fall back to a single block so non-empty buffers always render something
    // (e.g. a document of pure inline HTML or only whitespace).
    if blocks.is_empty() && !source.is_empty() {
        blocks.push(0..source.len());
    }

    blocks
}

impl Focusable for MarkdownEditor {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<()> for MarkdownEditor {}

impl Item for MarkdownEditor {
    type Event = ();

    fn tab_icon(&self, _window: &Window, _cx: &App) -> Option<Icon> {
        Some(Icon::new(IconName::FileDoc))
    }

    fn tab_content_text(&self, _detail: usize, _cx: &App) -> SharedString {
        self.title.clone()
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
        let style = MarkdownStyle::themed(MarkdownFont::Editor, window, cx);
        let active = self
            .active
            .as_ref()
            .map(|active| (active.index, active.editor.clone()));
        let this = cx.entity().downgrade();

        let blocks = self
            .blocks
            .iter()
            .enumerate()
            .map(|(index, block)| {
                if let Some((active_index, editor)) = active.as_ref()
                    && *active_index == index
                {
                    div()
                        .id(("markdown-block-edit", index))
                        .w_full()
                        .child(editor.clone())
                        .into_any_element()
                } else {
                    let this = this.clone();
                    div()
                        .id(("markdown-block", index))
                        .w_full()
                        .child(
                            MarkdownElement::new(block.markdown.clone(), style.clone())
                                .on_url_click(|url, _window, cx| cx.open_url(&url))
                                .on_source_click(move |source_index, _click_count, window, cx| {
                                    this.update(cx, |this, cx| {
                                        this.enter_edit(index, source_index, window, cx);
                                    })
                                    .ok();
                                    true
                                }),
                        )
                        .into_any_element()
                }
            })
            .collect::<Vec<_>>();

        v_flex()
            .id("markdown-editor")
            .track_focus(&self.focus_handle)
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll_handle)
            .bg(cx.theme().colors().editor_background)
            .px_8()
            .py_6()
            .gap_3()
            .children(blocks)
    }
}

/// Open `abs_path` as a [`MarkdownEditor`] in the workspace's active pane,
/// activating an existing tab for the same path if one is already open.
///
/// Falls back to the normal `open_paths` flow if the path can't be resolved to
/// a project path.
pub fn open_markdown_in_editor(
    workspace: &mut Workspace,
    abs_path: &Path,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let project = workspace.project().clone();
    let Some(project_path) = project.read(cx).project_path_for_absolute_path(abs_path, cx) else {
        workspace
            .open_paths(
                vec![abs_path.to_path_buf()],
                OpenOptions::default(),
                None,
                window,
                cx,
            )
            .detach();
        return;
    };

    // Reactivate an already-open markdown editor for this path.
    let pane = workspace.active_pane().clone();
    let existing_index = {
        let pane = pane.read(cx);
        pane.items_of_type::<MarkdownEditor>()
            .find(|item| item.read(cx).project_path.as_ref() == Some(&project_path))
            .and_then(|item| pane.index_for_item(&item))
    };
    if let Some(index) = existing_index {
        pane.update(cx, |pane, cx| {
            pane.activate_item(index, true, true, window, cx);
        });
        return;
    }

    let title: SharedString = abs_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "untitled.md".to_string())
        .into();
    let language_registry = project.read(cx).languages().clone();
    let buffer_task =
        project.update(cx, |project, cx| project.open_buffer(project_path.clone(), cx));

    cx.spawn_in(window, async move |workspace, cx| {
        let buffer = buffer_task.await?;
        workspace.update_in(cx, |workspace, window, cx| {
            let editor = cx.new(|cx| {
                MarkdownEditor::new(
                    buffer,
                    project.clone(),
                    Some(project_path.clone()),
                    title.clone(),
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

/// No-op today; reserved for registering actions in later milestones (toggle,
/// formatting toolbar commands). Called from the binary's startup so the wiring
/// is in place.
pub fn init(_cx: &mut App) {}

#[cfg(test)]
mod tests {
    use super::split_root_blocks;

    fn block_texts(source: &str) -> Vec<&str> {
        split_root_blocks(source)
            .into_iter()
            .map(|range| &source[range])
            .collect()
    }

    #[test]
    fn splits_top_level_blocks() {
        let source = "# Title\n\nA paragraph.\n\n- one\n- two\n";
        let blocks = block_texts(source);
        assert_eq!(blocks.len(), 3, "heading, paragraph, list");
        assert!(blocks[0].starts_with("# Title"));
        assert!(blocks[1].starts_with("A paragraph"));
        // The whole list is a single top-level block (not one block per item).
        assert!(blocks[2].contains("- one") && blocks[2].contains("- two"));
    }

    #[test]
    fn fenced_code_is_one_block() {
        let blocks = split_root_blocks("```rust\nfn main() {}\n```\n");
        assert_eq!(blocks.len(), 1);
    }

    #[test]
    fn empty_source_has_no_blocks() {
        assert!(split_root_blocks("").is_empty());
    }

    #[test]
    fn whitespace_only_falls_back_to_single_block() {
        // No markdown events, but a non-empty buffer should still render.
        assert_eq!(split_root_blocks("   \n  \n").len(), 1);
    }
}
