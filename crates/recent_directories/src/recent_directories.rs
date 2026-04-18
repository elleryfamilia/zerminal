use std::path::PathBuf;
use std::sync::Arc;

use active_terminal_cwd::{ActiveTerminalCwd, CwdChanged};
use chrono::{DateTime, Utc};
use fuzzy::{StringMatch, StringMatchCandidate, match_strings};
use gpui::{
    actions, App, AppContext, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable,
    IntoElement, Render, Task, WeakEntity, Window,
};
use picker::{Picker, PickerDelegate};
use serde::{Deserialize, Serialize};
use terminal_view::TerminalView;
use ui::{prelude::*, ListItem, ListItemSpacing};
use util::ResultExt as _;
use workspace::{ModalView, Workspace};

const MAX_RECENT_DIRS: usize = 50;
const KNOWN_SHELLS: &[&str] = &[
    "bash", "zsh", "fish", "sh", "dash", "nu", "elvish", "tcsh", "csh", "pwsh", "powershell",
];

actions!(
    recent_directories,
    [
        /// Opens the recent directories picker.
        Toggle
    ]
);

// --- Storage ---

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RecentDirectoryEntry {
    path: PathBuf,
    last_visited: DateTime<Utc>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct RecentDirectoriesStore {
    directories: Vec<RecentDirectoryEntry>,
}

fn storage_path() -> PathBuf {
    paths::data_dir().join("recent_directories.json")
}

fn load_store() -> RecentDirectoriesStore {
    let path = storage_path();
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|data| serde_json::from_str(&data).ok())
        .unwrap_or_default()
}

fn save_store(store: &RecentDirectoriesStore) {
    let path = storage_path();
    if let Ok(data) = serde_json::to_string_pretty(store) {
        if let Err(err) = std::fs::write(&path, data) {
            log::warn!("Failed to save recent directories: {err}");
        }
    }
}

fn record_directory(path: PathBuf) {
    let mut store = load_store();
    let now = Utc::now();

    if let Some(existing) = store.directories.iter_mut().find(|d| d.path == path) {
        existing.last_visited = now;
    } else {
        store.directories.push(RecentDirectoryEntry {
            path,
            last_visited: now,
        });
    }

    // Sort by most recently visited
    store
        .directories
        .sort_by(|a, b| b.last_visited.cmp(&a.last_visited));

    // Cap at max entries
    store.directories.truncate(MAX_RECENT_DIRS);

    save_store(&store);
}

fn load_directories() -> Vec<RecentDirectoryEntry> {
    let mut store = load_store();
    // Filter out directories that no longer exist
    store.directories.retain(|d| d.path.exists());
    store.directories
}

// --- Shell guard ---

fn is_terminal_at_shell(workspace: &Workspace, cx: &App) -> bool {
    workspace
        .active_item_as::<TerminalView>(cx)
        .map(|tv| {
            tv.read(cx)
                .terminal()
                .read(cx)
                .foreground_process_name()
                .map(|name| KNOWN_SHELLS.contains(&name.as_str()))
                .unwrap_or(false)
        })
        .unwrap_or(false)
}

fn shell_escape(path: &std::path::Path) -> String {
    let s = path.to_string_lossy();
    format!("'{}'", s.replace('\'', "'\\''"))
}

// --- Picker ---

pub struct RecentDirectoriesPicker {
    picker: Entity<Picker<RecentDirectoriesDelegate>>,
}

impl ModalView for RecentDirectoriesPicker {}
impl EventEmitter<DismissEvent> for RecentDirectoriesPicker {}

impl Focusable for RecentDirectoriesPicker {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.picker.focus_handle(cx)
    }
}

impl Render for RecentDirectoriesPicker {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .key_context("RecentDirectoriesPicker")
            .w(rems(34.))
            .child(self.picker.clone())
    }
}

impl RecentDirectoriesPicker {
    fn new(
        delegate: RecentDirectoriesDelegate,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let picker = cx.new(|cx| Picker::uniform_list(delegate, window, cx));
        Self { picker }
    }
}

struct RecentDirectoriesDelegate {
    workspace: WeakEntity<Workspace>,
    entries: Vec<RecentDirectoryEntry>,
    matches: Vec<StringMatch>,
    selected_index: usize,
    picker_view: WeakEntity<RecentDirectoriesPicker>,
}

impl RecentDirectoriesDelegate {
    fn new(
        workspace: WeakEntity<Workspace>,
        picker_view: WeakEntity<RecentDirectoriesPicker>,
    ) -> Self {
        let entries = load_directories();
        let matches = entries
            .iter()
            .enumerate()
            .map(|(ix, entry)| StringMatch {
                candidate_id: ix,
                string: entry
                    .path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default(),
                positions: Vec::new(),
                score: 0.0,
            })
            .collect();

        Self {
            workspace,
            entries,
            matches,
            selected_index: 0,
            picker_view,
        }
    }
}

impl PickerDelegate for RecentDirectoriesDelegate {
    type ListItem = ListItem;

    fn match_count(&self) -> usize {
        self.matches.len()
    }

    fn selected_index(&self) -> usize {
        self.selected_index
    }

    fn set_selected_index(
        &mut self,
        ix: usize,
        _window: &mut Window,
        _cx: &mut Context<Picker<Self>>,
    ) {
        self.selected_index = ix;
    }

    fn placeholder_text(&self, _window: &mut Window, _cx: &mut App) -> Arc<str> {
        "Switch to directory...".into()
    }

    fn update_matches(
        &mut self,
        query: String,
        window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> Task<()> {
        let background = cx.background_executor().clone();
        let candidates: Vec<_> = self
            .entries
            .iter()
            .enumerate()
            .map(|(id, entry)| {
                let label = format!(
                    "{} {}",
                    entry
                        .path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default(),
                    entry.path.display()
                );
                StringMatchCandidate::new(id, &label)
            })
            .collect();

        cx.spawn_in(window, async move |this, cx| {
            let matches = if query.is_empty() {
                candidates
                    .into_iter()
                    .enumerate()
                    .map(|(index, candidate)| StringMatch {
                        candidate_id: index,
                        string: candidate.string,
                        positions: Vec::new(),
                        score: 0.0,
                    })
                    .collect()
            } else {
                match_strings(
                    &candidates,
                    &query,
                    false,
                    true,
                    100,
                    &Default::default(),
                    background,
                )
                .await
            };

            this.update(cx, |this, _cx| {
                this.delegate.matches = matches;
                this.delegate.selected_index = 0;
            })
            .log_err();
        })
    }

    fn confirm(
        &mut self,
        _secondary: bool,
        _window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) {
        let Some(mat) = self.matches.get(self.selected_index) else {
            return;
        };
        let entry = &self.entries[mat.candidate_id];
        let path = entry.path.clone();

        if let Some(workspace) = self.workspace.upgrade() {
            workspace.update(cx, |workspace, cx| {
                if let Some(terminal_view) = workspace.active_item_as::<TerminalView>(cx) {
                    let terminal = terminal_view.read(cx).terminal().clone();
                    terminal.update(cx, |term, _cx| {
                        let cmd = format!("cd {}\n", shell_escape(&path));
                        term.input(cmd.into_bytes());
                    });
                }
            });
        }

        self.picker_view
            .update(cx, |_, cx| cx.emit(DismissEvent))
            .ok();
    }

    fn dismissed(&mut self, _window: &mut Window, cx: &mut Context<Picker<Self>>) {
        self.picker_view
            .update(cx, |_, cx| cx.emit(DismissEvent))
            .log_err();
    }

    fn render_match(
        &self,
        ix: usize,
        selected: bool,
        _window: &mut Window,
        _cx: &mut Context<Picker<Self>>,
    ) -> Option<Self::ListItem> {
        let mat = self.matches.get(ix)?;
        let entry = &self.entries[mat.candidate_id];
        let dir_name = entry
            .path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let full_path = entry.path.to_string_lossy().to_string();

        Some(
            ListItem::new(ix)
                .inset(true)
                .spacing(ListItemSpacing::Sparse)
                .toggle_state(selected)
                .child(
                    v_flex()
                        .child(Label::new(dir_name).size(LabelSize::Default))
                        .child(
                            Label::new(full_path)
                                .size(LabelSize::XSmall)
                                .color(Color::Muted),
                        ),
                ),
        )
    }
}

// --- Init ---

fn toggle_picker(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    if !is_terminal_at_shell(workspace, cx) {
        return;
    }

    let weak_workspace = workspace.weak_handle();
    workspace.toggle_modal(window, cx, |window, cx| {
        let delegate =
            RecentDirectoriesDelegate::new(weak_workspace, cx.entity().downgrade());
        RecentDirectoriesPicker::new(delegate, window, cx)
    });
}

pub fn init(cx: &mut App) {
    cx.observe_new(
        |workspace: &mut Workspace, _window, cx: &mut Context<Workspace>| {
            workspace.register_action(|workspace, _: &Toggle, window, cx| {
                toggle_picker(workspace, window, cx);
            });

            // Record git root directories when this workspace's CWD changes.
            let workspace_id = cx.entity_id();
            if let Some(tracker) = ActiveTerminalCwd::for_workspace(workspace_id, cx) {
                cx.subscribe(&tracker, |_workspace, tracker, _event: &CwdChanged, cx| {
                    if let Some(git_root) = tracker.read(cx).git_root() {
                        record_directory(git_root.to_path_buf());
                    }
                })
                .detach();
            }
        },
    )
    .detach();
}
