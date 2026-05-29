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

use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement, IntoElement,
    ParentElement, Render, ScrollHandle, SharedString, Styled, Subscription, Window,
};
use language::{Buffer, BufferEvent, LanguageRegistry};
use markdown::{Markdown, MarkdownElement, MarkdownFont, MarkdownStyle};
use project::{Project, ProjectPath};
use ui::prelude::*;
use ui::{Icon, IconName};
use workspace::item::{Item, ItemBufferKind, ItemEvent};
use workspace::{OpenOptions, Workspace};

/// One top-level markdown block: its byte range in the buffer's source and a
/// `Markdown` entity holding that block's substring for rendering.
struct Block {
    #[allow(dead_code)] // range is used starting in milestone 2 (click-to-edit).
    range: Range<usize>,
    markdown: Entity<Markdown>,
}

pub struct MarkdownEditor {
    buffer: Entity<Buffer>,
    #[allow(dead_code)] // used for saving starting in a later milestone.
    project: Entity<Project>,
    project_path: Option<ProjectPath>,
    title: SharedString,
    language_registry: Arc<LanguageRegistry>,
    focus_handle: FocusHandle,
    scroll_handle: ScrollHandle,
    blocks: Vec<Block>,
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
            if matches!(event, BufferEvent::Edited { .. } | BufferEvent::Reloaded) {
                this.rebuild_blocks(cx);
                cx.notify();
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

    fn to_item_events(_event: &Self::Event, _f: &mut dyn FnMut(ItemEvent)) {}

    fn buffer_kind(&self, _cx: &App) -> ItemBufferKind {
        ItemBufferKind::Singleton
    }
}

impl Render for MarkdownEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let style = MarkdownStyle::themed(MarkdownFont::Editor, window, cx);
        let blocks = self
            .blocks
            .iter()
            .map(|block| {
                MarkdownElement::new(block.markdown.clone(), style.clone())
                    .on_url_click(|url, _window, cx| cx.open_url(&url))
                    .into_any_element()
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
