use std::path::{Path, PathBuf};

use gpui::{App, AppContext, Context, Entity, EventEmitter, Global, Subscription, WeakEntity};
use gpui::Action as _;
use terminal::Terminal;
use terminal_view::TerminalView;
use workspace::{self, NewCenterTerminal, Workspace};

pub struct CwdChanged;

pub struct ActiveTerminalCwd {
    current_cwd: Option<PathBuf>,
    git_root: Option<PathBuf>,
    project_root: Option<PathBuf>,
    workspace: Option<WeakEntity<Workspace>>,
    _terminal_observation: Option<Subscription>,
}

impl EventEmitter<CwdChanged> for ActiveTerminalCwd {}

struct GlobalActiveCwd(Entity<ActiveTerminalCwd>);
impl Global for GlobalActiveCwd {}

impl ActiveTerminalCwd {
    pub fn global(cx: &App) -> Entity<Self> {
        cx.global::<GlobalActiveCwd>().0.clone()
    }

    pub fn try_global(cx: &App) -> Option<Entity<Self>> {
        cx.try_global::<GlobalActiveCwd>()
            .map(|g| g.0.clone())
    }

    pub fn current_cwd(&self) -> Option<&Path> {
        self.current_cwd.as_deref()
    }

    pub fn is_git_repo(&self) -> bool {
        self.git_root.is_some()
    }

    pub fn git_root(&self) -> Option<&Path> {
        self.git_root.as_deref()
    }

    pub fn project_root(&self) -> Option<&Path> {
        self.project_root.as_deref()
    }

    fn handle_active_item_changed(
        &mut self,
        workspace: &Entity<Workspace>,
        cx: &mut Context<Self>,
    ) {
        let workspace_ref = workspace.read(cx);

        if let Some(terminal_view) = workspace_ref.active_item_as::<TerminalView>(cx) {
            let terminal = terminal_view.read(cx).terminal().clone();
            self.update_cwd_from_terminal(&terminal, cx);

            self._terminal_observation = Some(cx.observe(&terminal, |this, terminal, cx| {
                this.update_cwd_from_terminal(&terminal, cx);
            }));
        } else {
            self._terminal_observation = None;
        }
    }

    fn update_cwd_from_terminal(
        &mut self,
        terminal: &Entity<Terminal>,
        cx: &mut Context<Self>,
    ) {
        let new_cwd = terminal.read(cx).working_directory();
        if new_cwd != self.current_cwd {
            self.current_cwd = new_cwd;
            self.git_root = self.current_cwd.as_ref().and_then(|p| find_git_root(p));

            // Only switch worktrees for git repos — non-git directories
            // (like ~) would cause expensive full-tree scans.
            let new_project_root = self.git_root.clone();

            if new_project_root != self.project_root {
                self.project_root = new_project_root;
                self.update_workspace_worktrees(cx);
            }

            cx.emit(CwdChanged);
            cx.notify();
        }
    }

    fn update_workspace_worktrees(&self, cx: &mut Context<Self>) {
        let Some(root) = self.project_root.clone() else {
            return;
        };
        let Some(workspace) = self.workspace.as_ref().and_then(|w| w.upgrade()) else {
            return;
        };

        let project = workspace.read(cx).project().clone();

        let mut root_already_exists = false;
        let mut ids_to_remove = Vec::new();

        for worktree in project.read(cx).visible_worktrees(cx) {
            let worktree_ref = worktree.read(cx);
            if worktree_ref.abs_path().as_ref() == root.as_path() {
                root_already_exists = true;
            } else {
                ids_to_remove.push(worktree_ref.id());
            }
        }

        if !ids_to_remove.is_empty() {
            project.update(cx, |project, cx| {
                for id in ids_to_remove {
                    project.remove_worktree(id, cx);
                }
            });
        }

        if !root_already_exists {
            let task = project.update(cx, |project, cx| {
                project.find_or_create_worktree(&root, true, cx)
            });

            cx.spawn(async move |_this, _cx| {
                task.await?;
                anyhow::Ok(())
            })
            .detach_and_log_err(cx);
        }
    }
}

fn find_git_root(path: &Path) -> Option<PathBuf> {
    let mut current = Some(path);
    while let Some(dir) = current {
        if dir.join(".git").exists() {
            return Some(dir.to_path_buf());
        }
        current = dir.parent();
    }
    None
}

pub fn init(cx: &mut App) {
    let entity = cx.new(|_cx| ActiveTerminalCwd {
        current_cwd: None,
        git_root: None,
        project_root: None,
        workspace: None,
        _terminal_observation: None,
    });
    cx.set_global(GlobalActiveCwd(entity));

    cx.observe_new(
        |_workspace: &mut Workspace, window, cx: &mut Context<Workspace>| {
            let Some(window) = window else { return };
            let workspace_entity = cx.entity();

            let global = ActiveTerminalCwd::global(cx);
            global.update(cx, |this, _cx| {
                this.workspace = Some(workspace_entity.downgrade());
            });

            // Open a terminal on startup if center pane is empty
            cx.defer_in(window, |workspace, window, cx| {
                if workspace.active_pane().read(cx).items_len() == 0 {
                    window.dispatch_action(
                        NewCenterTerminal::default().boxed_clone(),
                        cx,
                    );
                }
            });

            window
                .subscribe(&workspace_entity, cx, move |workspace, event, _window, cx| {
                    if matches!(event, workspace::Event::ActiveItemChanged) {
                        let global = ActiveTerminalCwd::global(cx);
                        global.update(cx, |this, cx| {
                            this.handle_active_item_changed(&workspace, cx);
                        });
                    }
                })
                .detach();
        },
    )
    .detach();
}
