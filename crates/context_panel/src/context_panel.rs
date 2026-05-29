use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use active_terminal_cwd::ActiveTerminalCwd;
use anyhow::Result;
use coding_tools::{KNOWN_TOOLS, MemoryFileSpec, SHARED_PROJECT_FILES};
use gpui::{
    Action, App, AsyncWindowContext, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement, IntoElement, ParentElement, Pixels, Render, Styled, Subscription,
    WeakEntity, Window, actions, px,
};
use project::git_store::GitStoreEvent;
use settings::Settings;
use ui::prelude::*;
use workspace::{
    OpenOptions, Workspace,
    dock::{DockPosition, Panel, PanelEvent},
};
use worktree::WorktreeSettings;

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
    tool: Option<SharedString>,
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
    _subscriptions: Vec<Subscription>,
}

impl ContextPanel {
    pub fn new(workspace: &Workspace, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut memory_files = Vec::new();
        let mut project_docs = Vec::new();
        let mut is_git_repo = false;
        let mut subscriptions = Vec::new();

        let workspace_id = workspace.weak_handle().entity_id();
        if let Some(cwd_entity) = ActiveTerminalCwd::for_workspace(workspace_id, cx) {
            let tracker = cwd_entity.read(cx);
            if tracker.is_git_repo()
                && let Some(project_root) = tracker.project_root()
            {
                is_git_repo = true;
                discover_memory_files(project_root, &mut memory_files);
                discover_project_docs(project_root, &mut project_docs, cx);
            }

            subscriptions.push(cx.observe(&cwd_entity, |this, cwd_tracker, cx| {
                let tracker = cwd_tracker.read(cx);
                let in_repo = tracker.is_git_repo();
                let was_in_repo = this.is_git_repo;
                this.memory_files.clear();
                this.project_docs.clear();
                if in_repo
                    && let Some(project_root) = tracker.project_root()
                {
                    this.is_git_repo = true;
                    discover_memory_files(project_root, &mut this.memory_files);
                    discover_project_docs(project_root, &mut this.project_docs, cx);
                    this.collapsed_dirs = collapsed_subdirs(&this.project_docs);
                } else {
                    this.is_git_repo = false;
                    this.collapsed_dirs.clear();
                    if was_in_repo {
                        cx.emit(PanelEvent::Close);
                    }
                }
                cx.notify();
            }));
        }

        // Re-render when the project's git repositories change so the panel
        // icon picks up newly-discovered repos without waiting for the
        // active-terminal CWD tracker to settle.
        let git_store = workspace.project().read(cx).git_store().clone();
        subscriptions.push(cx.subscribe(&git_store, |_, _, event, cx| match event {
            GitStoreEvent::RepositoryAdded
            | GitStoreEvent::RepositoryRemoved(_)
            | GitStoreEvent::ActiveRepositoryChanged(_) => {
                cx.notify();
            }
            _ => {}
        }));

        let collapsed_dirs = collapsed_subdirs(&project_docs);
        Self {
            workspace: workspace.weak_handle(),
            focus_handle: cx.focus_handle(),
            memory_files,
            project_docs,
            is_git_repo,
            collapsed_dirs,
            _subscriptions: subscriptions,
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
            let is_markdown = path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| {
                    extension.eq_ignore_ascii_case("md")
                        || extension.eq_ignore_ascii_case("markdown")
                });
            workspace.update(cx, |workspace, cx| {
                if is_markdown {
                    markdown_editor::open_markdown_in_editor(workspace, &path, window, cx);
                } else {
                    workspace
                        .open_paths(vec![path], OpenOptions::default(), None, window, cx)
                        .detach();
                }
            });
        }
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let cwd_entity = self
            .workspace
            .upgrade()
            .and_then(|w| ActiveTerminalCwd::for_workspace(w.entity_id(), cx));
        if let Some(cwd_entity) = cwd_entity {
            let tracker = cwd_entity.read(cx);
            self.memory_files.clear();
            self.project_docs.clear();
            if tracker.is_git_repo()
                && let Some(project_root) = tracker.project_root()
            {
                self.is_git_repo = true;
                discover_memory_files(project_root, &mut self.memory_files);
                discover_project_docs(project_root, &mut self.project_docs, cx);
            } else {
                self.is_git_repo = false;
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

    fn render_tool_tag(tool: &Option<SharedString>, _cx: &Context<Self>) -> impl IntoElement {
        div().when_some(tool.clone(), |el, name| {
            el.px_1()
                .rounded_sm()
                .child(Label::new(name).size(LabelSize::XSmall).color(Color::Muted))
        })
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
                            .child(Self::render_tool_tag(&file.tool, cx))
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

    fn icon(&self, _window: &Window, cx: &App) -> Option<IconName> {
        // Drive icon visibility off the project's discovered git repositories
        // rather than the active-terminal CWD, so the button appears as soon as
        // the workspace opens on a git repo regardless of which pane is focused.
        let in_git_repo = self
            .workspace
            .upgrade()
            .map(|w| !w.read(cx).project().read(cx).repositories(cx).is_empty())
            .unwrap_or(false);
        in_git_repo.then_some(IconName::Book)
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
        8
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
    for filename in SHARED_PROJECT_FILES {
        let path = git_root.join(filename);
        if path.exists() {
            files.push(MemoryFile {
                path,
                label: (*filename).into(),
                scope: FileScope::Project,
                tool: None,
            });
        }
    }

    for tool in KNOWN_TOOLS {
        for spec in tool.project_memory {
            discover_from_spec(git_root, spec, FileScope::Project, Some(tool.name), files);
        }

        if let Some(home) = dirs::home_dir() {
            for spec in tool.global_memory {
                discover_from_spec(&home, spec, FileScope::Global, Some(tool.name), files);
            }

        }
    }
}

fn discover_from_spec(
    base: &Path,
    spec: &MemoryFileSpec,
    scope: FileScope,
    tool_name: Option<&'static str>,
    files: &mut Vec<MemoryFile>,
) {
    let target = base.join(spec.pattern);
    if spec.is_dir {
        if target.is_dir() {
            scan_dir_for_md(&target, scope, tool_name, files);
            for subdir in spec.subdirs {
                let sub = target.join(subdir);
                if sub.is_dir() {
                    scan_dir_for_md(&sub, scope, tool_name, files);
                }
            }
        }
    } else if target.exists() {
        files.push(MemoryFile {
            path: target,
            label: spec.pattern.into(),
            scope,
            tool: tool_name.map(SharedString::from),
        });
    }
}

fn scan_dir_for_md(
    dir: &Path,
    scope: FileScope,
    tool_name: Option<&'static str>,
    files: &mut Vec<MemoryFile>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() && path.extension().is_some_and(|ext| ext == "md") {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            files.push(MemoryFile {
                path,
                label: name.into(),
                scope,
                tool: tool_name.map(SharedString::from),
            });
        }
    }
}

const SKIP_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "vendor",
    "venv",
    ".venv",
    "env",
    "__pycache__",
    "site-packages",
    "dist",
    "build",
    ".next",
    ".nuxt",
    ".turbo",
    ".cache",
];

fn is_nested_vcs_root(path: &Path, git_root: &Path) -> bool {
    path != git_root && path.join(".git").exists()
}

fn collapsed_subdirs(docs: &[ProjectDoc]) -> HashSet<SharedString> {
    docs.iter()
        .map(|d| d.directory.clone())
        .filter(|d| !d.is_empty())
        .collect()
}

fn discover_project_docs(git_root: &Path, docs: &mut Vec<ProjectDoc>, cx: &App) {
    let settings = WorktreeSettings::get_global(cx).clone();
    walk_for_markdown(git_root, &settings, docs);
}

fn walk_for_markdown(git_root: &Path, settings: &WorktreeSettings, docs: &mut Vec<ProjectDoc>) {
    let git_root_buf = git_root.to_path_buf();
    let walker = ignore::WalkBuilder::new(git_root)
        .standard_filters(true)
        .require_git(true)
        .same_file_system(true)
        .filter_entry(move |entry| {
            let path = entry.path();
            if is_nested_vcs_root(path, &git_root_buf) {
                return false;
            }
            if let Some(name) = path.file_name() {
                let name_str = name.to_string_lossy();
                if SKIP_DIRS.iter().any(|d| *d == name_str.as_ref()) {
                    return false;
                }
            }
            true
        })
        .build();

    for entry in walker {
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        let path = entry.path();
        if !path.extension().is_some_and(|ext| ext == "md") {
            continue;
        }
        let rel_path = path.strip_prefix(git_root).unwrap_or(path);
        if settings.file_scan_exclusions.is_match_std_path(rel_path) {
            continue;
        }
        let name = rel_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let directory = rel_path
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        docs.push(ProjectDoc {
            path: path.to_path_buf(),
            name: name.into(),
            directory: directory.into(),
        });
    }
}
