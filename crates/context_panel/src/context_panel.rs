use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use active_terminal_cwd::{ActiveTerminalCwd, CwdChanged};
use anyhow::Result;
use gpui::{
    Action, App, AsyncWindowContext, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement, IntoElement, ParentElement, Pixels, Render, Styled, Subscription,
    WeakEntity, Window, actions, px,
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

#[derive(Clone, Copy, PartialEq)]
enum FileScope {
    Project,
    Global,
}

#[derive(Clone)]
struct MemoryFile {
    path: PathBuf,
    label: SharedString,
    scope: FileScope,
}

#[derive(Clone)]
struct ProjectDoc {
    path: PathBuf,
    name: SharedString,
    directory: SharedString,
}

pub struct ContextPanel {
    workspace: WeakEntity<Workspace>,
    focus_handle: FocusHandle,
    memory_files: Vec<MemoryFile>,
    project_docs: Vec<ProjectDoc>,
    is_git_repo: bool,
    collapsed_dirs: HashSet<SharedString>,
    _cwd_subscription: Option<Subscription>,
}

impl ContextPanel {
    pub fn new(workspace: &Workspace, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut memory_files = Vec::new();
        let mut project_docs = Vec::new();
        let mut is_git_repo = false;
        let mut cwd_subscription = None;

        if let Some(cwd_entity) = ActiveTerminalCwd::try_global(cx) {
            let tracker = cwd_entity.read(cx);
            is_git_repo = tracker.is_git_repo();
            if let Some(git_root) = tracker.git_root() {
                discover_memory_files(git_root, &mut memory_files);
                discover_project_docs(git_root, &mut project_docs);
            }

            cwd_subscription = Some(
                cx.subscribe(&cwd_entity, |this, cwd_tracker, _event: &CwdChanged, cx| {
                    let tracker = cwd_tracker.read(cx);
                    this.is_git_repo = tracker.is_git_repo();
                    this.memory_files.clear();
                    this.project_docs.clear();
                    if let Some(git_root) = tracker.git_root() {
                        discover_memory_files(git_root, &mut this.memory_files);
                        discover_project_docs(git_root, &mut this.project_docs);
                    }
                    cx.notify();
                }),
            );
        }

        Self {
            workspace: workspace.weak_handle(),
            focus_handle: cx.focus_handle(),
            memory_files,
            project_docs,
            is_git_repo,
            collapsed_dirs: HashSet::new(),
            _cwd_subscription: cwd_subscription,
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

    fn open_path(&self, path: &Path, window: &mut Window, cx: &mut Context<Self>) {
        let path = path.to_path_buf();
        if let Some(workspace) = self.workspace.upgrade() {
            workspace.update(cx, |workspace, cx| {
                workspace
                    .open_paths(vec![path], OpenOptions::default(), None, window, cx)
                    .detach();
            });
        }
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        if let Some(cwd_entity) = ActiveTerminalCwd::try_global(cx) {
            let tracker = cwd_entity.read(cx);
            self.is_git_repo = tracker.is_git_repo();
            self.memory_files.clear();
            self.project_docs.clear();
            if let Some(git_root) = tracker.git_root() {
                discover_memory_files(git_root, &mut self.memory_files);
                discover_project_docs(git_root, &mut self.project_docs);
            }
        }
        cx.notify();
    }

    fn render_section_header(
        title: &str,
        count: usize,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(cx.theme().colors().border)
            .child(
                Label::new(format!("{title} ({count})"))
                    .size(LabelSize::Small)
                    .color(Color::Muted),
            )
    }

    fn render_scope_tag(scope: FileScope, _cx: &Context<Self>) -> impl IntoElement {
        let (text, label_color) = match scope {
            FileScope::Project => ("project", Color::Accent),
            FileScope::Global => ("global", Color::Success),
        };
        div()
            .px_1()
            .rounded_sm()
            .child(Label::new(text).size(LabelSize::XSmall).color(label_color))
    }
}

impl EventEmitter<PanelEvent> for ContextPanel {}

impl Render for ContextPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let memory_files = self.memory_files.clone();
        let project_docs = self.project_docs.clone();

        // Group project docs by directory
        let mut doc_groups: BTreeMap<SharedString, Vec<ProjectDoc>> = BTreeMap::new();
        for doc in &project_docs {
            doc_groups
                .entry(doc.directory.clone())
                .or_default()
                .push(doc.clone());
        }

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(cx.theme().colors().panel_background)
            // Header
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
                        Label::new("Context")
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
                    .id("context-scroll")
                    .flex()
                    .flex_col()
                    .overflow_y_scroll()
                    .size_full()
                    // Memory section
                    .child(Self::render_section_header(
                        "Memory",
                        memory_files.len(),
                        cx,
                    ))
                    .children(memory_files.iter().enumerate().map(|(ix, file)| {
                        let file_clone = file.clone();
                        div()
                            .id(("mem", ix))
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .px_2()
                            .py_0p5()
                            .cursor_pointer()
                            .hover(|s| s.bg(cx.theme().colors().ghost_element_hover))
                            .child(Self::render_scope_tag(file.scope, cx))
                            .child(
                                Icon::new(IconName::FileDoc)
                                    .size(IconSize::Small)
                                    .color(Color::Muted),
                            )
                            .child(
                                Label::new(file.label.clone())
                                    .size(LabelSize::Small)
                                    .single_line(),
                            )
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_path(&file_clone.path, window, cx);
                            }))
                    }))
                    .when(memory_files.is_empty(), |el| {
                        el.child(
                            div().px_2().py_1().child(
                                Label::new("No context files found")
                                    .size(LabelSize::XSmall)
                                    .color(Color::Muted),
                            ),
                        )
                    })
                    // Project Documents section
                    .child(Self::render_section_header(
                        "Project Documents",
                        project_docs.len(),
                        cx,
                    ))
                    .children(doc_groups.iter().map(|(dir, docs)| {
                        let dir_label: SharedString = if dir.is_empty() {
                            "/".into()
                        } else {
                            dir.clone()
                        };
                        let is_collapsed = self.collapsed_dirs.contains(&dir_label);
                        let chevron_icon = if is_collapsed {
                            IconName::ChevronRight
                        } else {
                            IconName::ChevronDown
                        };
                        let toggle_dir = dir_label.clone();

                        div()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .id(SharedString::from(format!("dir-{}", dir)))
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .px_2()
                                    .pt_1()
                                    .cursor_pointer()
                                    .hover(|s| s.bg(cx.theme().colors().ghost_element_hover))
                                    .child(
                                        Icon::new(chevron_icon)
                                            .size(IconSize::XSmall)
                                            .color(Color::Muted),
                                    )
                                    .child(
                                        Icon::new(IconName::Folder)
                                            .size(IconSize::Small)
                                            .color(Color::Accent),
                                    )
                                    .child(
                                        Label::new(dir_label)
                                            .size(LabelSize::XSmall)
                                            .color(Color::Accent),
                                    )
                                    .on_click(cx.listener(move |this, _, _window, cx| {
                                        if this.collapsed_dirs.contains(&toggle_dir) {
                                            this.collapsed_dirs.remove(&toggle_dir);
                                        } else {
                                            this.collapsed_dirs.insert(toggle_dir.clone());
                                        }
                                        cx.notify();
                                    })),
                            )
                            .when(!is_collapsed, |el| {
                                el.children(docs.iter().enumerate().map(|(ix, doc)| {
                                    let doc_clone = doc.clone();
                                    div()
                                        .id(SharedString::from(format!("doc-{}-{ix}", dir)))
                                        .flex()
                                        .items_center()
                                        .gap_1()
                                        .pl(px(28.0))
                                        .pr_2()
                                        .py_0p5()
                                        .cursor_pointer()
                                        .hover(|s| {
                                            s.bg(cx.theme().colors().ghost_element_hover)
                                        })
                                        .child(
                                            Icon::new(IconName::FileDoc)
                                                .size(IconSize::Small)
                                                .color(Color::Muted),
                                        )
                                        .child(
                                            Label::new(doc.name.clone())
                                                .size(LabelSize::Small)
                                                .single_line(),
                                        )
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.open_path(&doc_clone.path, window, cx);
                                        }))
                                }))
                            })
                    }))
                    .when(project_docs.is_empty(), |el| {
                        el.child(
                            div().px_2().py_1().child(
                                Label::new("No markdown files in repo")
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
        px(260.0)
    }

    fn icon(&self, _window: &Window, _cx: &App) -> Option<IconName> {
        if self.is_git_repo {
            Some(IconName::Book)
        } else {
            None
        }
    }

    fn icon_tooltip(&self, _window: &Window, _cx: &App) -> Option<&'static str> {
        Some("Context")
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

// --- File discovery ---

fn discover_memory_files(git_root: &Path, files: &mut Vec<MemoryFile>) {
    // Project-level: CLAUDE.md at git root
    let claude_md = git_root.join("CLAUDE.md");
    if claude_md.exists() {
        files.push(MemoryFile {
            path: claude_md,
            label: "CLAUDE.md".into(),
            scope: FileScope::Project,
        });
    }

    // Project-level: AGENTS.md at git root
    let agents_md = git_root.join("AGENTS.md");
    if agents_md.exists() {
        files.push(MemoryFile {
            path: agents_md,
            label: "AGENTS.md".into(),
            scope: FileScope::Project,
        });
    }

    // Project-level: .claude/ directory
    let claude_dir = git_root.join(".claude");
    if claude_dir.is_dir() {
        scan_dir_for_md(&claude_dir, FileScope::Project, files);

        let memory_dir = claude_dir.join("memory");
        if memory_dir.is_dir() {
            scan_dir_for_md(&memory_dir, FileScope::Project, files);
        }

        let rules_dir = claude_dir.join("rules");
        if rules_dir.is_dir() {
            scan_dir_for_md(&rules_dir, FileScope::Project, files);
        }
    }

    // Global: ~/.claude/CLAUDE.md
    if let Some(home) = dirs::home_dir() {
        let global_claude = home.join(".claude").join("CLAUDE.md");
        if global_claude.exists() {
            files.push(MemoryFile {
                path: global_claude,
                label: "CLAUDE.md".into(),
                scope: FileScope::Global,
            });
        }

        let global_rules = home.join(".claude").join("rules");
        if global_rules.is_dir() {
            scan_dir_for_md(&global_rules, FileScope::Global, files);
        }

        // Auto-memory: ~/.claude/projects/<encoded-path>/memory/
        let encoded = encode_project_path(git_root);
        let auto_memory = home
            .join(".claude")
            .join("projects")
            .join(&encoded)
            .join("memory");
        if auto_memory.is_dir() {
            scan_dir_for_md(&auto_memory, FileScope::Global, files);
        }
    }
}

fn scan_dir_for_md(dir: &Path, scope: FileScope, files: &mut Vec<MemoryFile>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "md") {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            files.push(MemoryFile {
                path,
                label: name.into(),
                scope,
            });
        }
    }
}

fn discover_project_docs(git_root: &Path, docs: &mut Vec<ProjectDoc>) {
    walk_for_markdown(git_root, git_root, docs);
}

fn walk_for_markdown(dir: &Path, git_root: &Path, docs: &mut Vec<ProjectDoc>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    let mut dirs_to_recurse = Vec::new();

    for entry in entries.flatten() {
        let path = entry.path();
        let file_name = entry.file_name();
        let name_str = file_name.to_string_lossy();

        if path.is_dir() {
            // Skip hidden dirs, node_modules, target, .git
            if name_str.starts_with('.')
                || name_str == "node_modules"
                || name_str == "target"
                || name_str == "vendor"
            {
                continue;
            }
            dirs_to_recurse.push(path);
        } else if path.extension().is_some_and(|ext| ext == "md") {
            let rel_path = path.strip_prefix(git_root).unwrap_or(&path);
            let name = rel_path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let directory = rel_path
                .parent()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            docs.push(ProjectDoc {
                path,
                name: name.into(),
                directory: directory.into(),
            });
        }
    }

    for sub_dir in dirs_to_recurse {
        walk_for_markdown(&sub_dir, git_root, docs);
    }
}

fn encode_project_path(path: &Path) -> String {
    path.to_string_lossy().replace(['/', '_'], "-")
}
