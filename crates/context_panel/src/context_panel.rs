use std::path::PathBuf;

use anyhow::Result;
use gpui::{
    Action, App, AsyncWindowContext, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement, IntoElement, ParentElement, Pixels, Render, Styled, WeakEntity, Window,
    actions, px,
};
use ui::prelude::*;
use workspace::{
    OpenOptions, Workspace,
    dock::{DockPosition, Panel, PanelEvent},
};

const CONTEXT_PANEL_KEY: &str = "ContextPanel";

actions!(
    context_panel,
    [
        /// Toggles the context panel.
        Toggle,
        /// Toggles focus on the context panel.
        ToggleFocus
    ]
);

pub fn init(cx: &mut App) {
    cx.observe_new(
        |workspace: &mut Workspace, _window, _: &mut Context<Workspace>| {
            workspace.register_action(|workspace, _: &ToggleFocus, window, cx| {
                workspace.toggle_panel_focus::<ContextPanel>(window, cx);
            });
            workspace.register_action(|workspace, _: &Toggle, window, cx| {
                if !workspace.toggle_panel_focus::<ContextPanel>(window, cx) {
                    workspace.close_panel::<ContextPanel>(window, cx);
                }
            });
        },
    )
    .detach();
}

/// A file discovered as AI agent context.
#[derive(Clone)]
struct ContextFile {
    path: PathBuf,
    label: SharedString,
}

pub struct ContextPanel {
    workspace: WeakEntity<Workspace>,
    focus_handle: FocusHandle,
    files: Vec<ContextFile>,
}

impl ContextPanel {
    pub fn new(workspace: &Workspace, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            workspace: workspace.weak_handle(),
            focus_handle: cx.focus_handle(),
            files: Vec::new(),
        }
    }

    pub fn load(
        workspace: WeakEntity<Workspace>,
        cx: &mut AsyncWindowContext,
    ) -> gpui::Task<Result<Entity<Self>>> {
        cx.spawn(async move |cx| {
            workspace.update_in(cx, |workspace, window, cx| {
                cx.new(|cx| Self::new(workspace, window, cx))
            })
        })
    }

    fn discover_files_from_worktrees(&mut self, cx: &App) {
        self.files.clear();

        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        let project = workspace.read(cx).project().read(cx);

        for worktree_handle in project.visible_worktrees(cx) {
            let worktree = worktree_handle.read(cx);
            let root_path = worktree.abs_path();

            // Check for CLAUDE.md at worktree root
            let claude_md = root_path.join("CLAUDE.md");
            if claude_md.exists() {
                self.files.push(ContextFile {
                    path: claude_md,
                    label: "CLAUDE.md".into(),
                });
            }

            // Check for .claude/ directory
            let claude_dir = root_path.join(".claude");
            if claude_dir.is_dir() {
                if let Ok(entries) = std::fs::read_dir(&claude_dir) {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        if path.extension().is_some_and(|ext| ext == "md") {
                            let name = path
                                .file_name()
                                .map(|n| n.to_string_lossy().to_string())
                                .unwrap_or_default();
                            self.files.push(ContextFile {
                                path,
                                label: format!(".claude/{name}").into(),
                            });
                        }
                    }
                }

                // Check for memory files in .claude/memory/
                let memory_dir = claude_dir.join("memory");
                if memory_dir.is_dir() {
                    if let Ok(entries) = std::fs::read_dir(&memory_dir) {
                        for entry in entries.flatten() {
                            let path = entry.path();
                            if path.extension().is_some_and(|ext| ext == "md") {
                                let name = path
                                    .file_name()
                                    .map(|n| n.to_string_lossy().to_string())
                                    .unwrap_or_default();
                                self.files.push(ContextFile {
                                    path,
                                    label: format!(".claude/memory/{name}").into(),
                                });
                            }
                        }
                    }
                }
            }
        }
    }

    fn open_file(&self, file: &ContextFile, window: &mut Window, cx: &mut Context<Self>) {
        let path = file.path.clone();
        if let Some(workspace) = self.workspace.upgrade() {
            workspace.update(cx, |workspace, cx| {
                workspace
                    .open_paths(
                        vec![path],
                        OpenOptions::default(),
                        None,
                        window,
                        cx,
                    )
                    .detach();
            });
        }
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.discover_files_from_worktrees(cx);
        cx.notify();
    }
}

impl EventEmitter<PanelEvent> for ContextPanel {}

impl Render for ContextPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let files = self.files.clone();

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(cx.theme().colors().panel_background)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_2()
                    .py_1()
                    .border_b_1()
                    .border_color(cx.theme().colors().border)
                    .child(
                        Label::new("Agent Context")
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
                    .child(
                        IconButton::new("refresh", IconName::ArrowCircle)
                            .icon_size(IconSize::Small)
                            .on_click(cx.listener(|this, _, _window, cx| {
                                this.refresh(cx);
                            })),
                    ),
            )
            .child(
                div()
                    .id("context-file-list")
                    .flex()
                    .flex_col()
                    .overflow_y_scroll()
                    .size_full()
                    .children(files.iter().enumerate().map(|(ix, file)| {
                        let file_clone = file.clone();
                        div()
                            .id(ix)
                            .flex()
                            .items_center()
                            .gap_2()
                            .px_2()
                            .py_0p5()
                            .rounded_md()
                            .cursor_pointer()
                            .hover(|s| s.bg(cx.theme().colors().ghost_element_hover))
                            .child(
                                Icon::new(IconName::File)
                                    .size(IconSize::Small)
                                    .color(Color::Muted),
                            )
                            .child(
                                Label::new(file.label.clone())
                                    .size(LabelSize::Small),
                            )
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_file(&file_clone, window, cx);
                            }))
                    }))
                    .when(files.is_empty(), |this| {
                        this.child(
                            div()
                                .px_2()
                                .py_4()
                                .child(
                                    Label::new("No context files found")
                                        .size(LabelSize::Small)
                                        .color(Color::Muted),
                                )
                                .child(
                                    Label::new("Add a CLAUDE.md to your project root")
                                        .size(LabelSize::XSmall)
                                        .color(Color::Muted),
                                ),
                        )
                    }),
            )
    }
}

impl Focusable for ContextPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Panel for ContextPanel {
    fn position(&self, _window: &Window, _cx: &App) -> DockPosition {
        DockPosition::Left
    }

    fn position_is_valid(&self, position: DockPosition) -> bool {
        matches!(position, DockPosition::Left | DockPosition::Right)
    }

    fn set_position(
        &mut self,
        _position: DockPosition,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
    }

    fn default_size(&self, _window: &Window, _cx: &App) -> Pixels {
        px(240.0)
    }

    fn icon(&self, _window: &Window, _cx: &App) -> Option<IconName> {
        Some(IconName::Book)
    }

    fn icon_tooltip(&self, _window: &Window, _cx: &App) -> Option<&'static str> {
        Some("Agent Context")
    }

    fn toggle_action(&self) -> Box<dyn Action> {
        Box::new(Toggle)
    }

    fn persistent_name() -> &'static str {
        "ContextPanel"
    }

    fn panel_key() -> &'static str {
        CONTEXT_PANEL_KEY
    }

    fn activation_priority(&self) -> u32 {
        5
    }

    fn set_active(&mut self, active: bool, window: &mut Window, cx: &mut Context<Self>) {
        if active {
            cx.defer_in(window, |this, _window, cx| {
                this.refresh(cx);
            });
        }
    }
}
