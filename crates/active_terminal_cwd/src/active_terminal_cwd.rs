use std::path::{Path, PathBuf};

use db::kvp::KeyValueStore;
use gpui::{
    App, AppContext, Context, Entity, EntityId, EventEmitter, Global, Subscription, WeakEntity,
};
use gpui_util::ResultExt;
use terminal::Terminal;
use terminal_view::TerminalView;
use workspace::{self, NewCenterTerminal, Workspace};

const PROJECT_ROOT_KVP_KEY: &str = "active_terminal_cwd_project_root";

pub struct CwdChanged;

pub struct ProjectSwitchRequested {
    pub new_root: PathBuf,
    pub origin_workspace: EntityId,
}

pub struct ActiveTerminalCwd {
    current_cwd: Option<PathBuf>,
    git_root: Option<PathBuf>,
    project_root: Option<PathBuf>,
    pending_project_root: Option<PathBuf>,
    switch_generation: u64,
    workspace: Option<WeakEntity<Workspace>>,
    needs_restore: bool,
    _terminal_observation: Option<Subscription>,
}

impl EventEmitter<CwdChanged> for ActiveTerminalCwd {}
impl EventEmitter<ProjectSwitchRequested> for ActiveTerminalCwd {}

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
        let origin_workspace = workspace.entity_id();
        let workspace_ref = workspace.read(cx);

        if let Some(terminal_view) = workspace_ref.active_item_as::<TerminalView>(cx) {
            let terminal = terminal_view.read(cx).terminal().clone();
            self.update_cwd_from_terminal(&terminal, origin_workspace, cx);

            self._terminal_observation =
                Some(cx.observe(&terminal, move |this, terminal, cx| {
                    this.update_cwd_from_terminal(&terminal, origin_workspace, cx);
                }));
        } else {
            self._terminal_observation = None;
        }
    }

    fn update_cwd_from_terminal(
        &mut self,
        terminal: &Entity<Terminal>,
        origin_workspace: EntityId,
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
                match (self.project_root.as_ref(), new_project_root) {
                    (Some(current_root), Some(new_root)) => {
                        // Stay in the current workspace when the terminal is
                        // still inside its tree, even if the nearest `.git`
                        // changed (e.g. entering a nested submodule).
                        let still_in_tree = self
                            .current_cwd
                            .as_deref()
                            .is_some_and(|cwd| cwd.starts_with(current_root));
                        if !still_in_tree {
                            self.pending_project_root = Some(new_root.clone());
                            self.switch_generation += 1;
                            cx.emit(ProjectSwitchRequested {
                                new_root,
                                origin_workspace,
                            });
                        }
                    }
                    (None, Some(new_root)) => {
                        // Initial project setup — switch immediately.
                        self.project_root = Some(new_root);
                        self.save_project_root(cx);
                        self.update_workspace_worktrees(cx);
                    }
                    (_, None) => {
                        // CDing away from a git repo (e.g., cd ~) — no
                        // confirmation needed since there's nothing to
                        // switch to.
                        self.project_root = None;
                        self.pending_project_root = None;
                        self.save_project_root(cx);
                        self.update_workspace_worktrees(cx);
                    }
                }
            }

            cx.emit(CwdChanged);
            cx.notify();
        }
    }

    pub fn switch_generation(&self) -> u64 {
        self.switch_generation
    }

    pub fn execute_worktree_switch(
        &mut self,
        new_root: PathBuf,
        generation: u64,
        cx: &mut Context<Self>,
    ) {
        if generation != self.switch_generation {
            return;
        }
        self.project_root = Some(new_root);
        self.pending_project_root = None;
        self.save_project_root(cx);
        self.update_workspace_worktrees(cx);
    }

    pub fn cancel_worktree_switch(&mut self, generation: u64) {
        if generation != self.switch_generation {
            return;
        }
        self.pending_project_root = None;
    }

    fn save_project_root(&self, cx: &App) {
        let db = KeyValueStore::global(cx);
        match &self.project_root {
            Some(root) => {
                let root_str = root.to_string_lossy().to_string();
                db::write_and_log(cx, move || async move {
                    db.write_kvp(
                        PROJECT_ROOT_KVP_KEY.to_string(),
                        root_str,
                    )
                    .await
                });
            }
            None => {
                db::write_and_log(cx, move || async move {
                    db.delete_kvp(PROJECT_ROOT_KVP_KEY.to_string()).await
                });
            }
        }
    }

    fn restore_project_root(&mut self, cx: &mut Context<Self>) {
        if !self.needs_restore {
            return;
        }
        self.needs_restore = false;

        let db = KeyValueStore::global(cx);
        if let Some(root_str) = db.read_kvp(PROJECT_ROOT_KVP_KEY).log_err().flatten() {
            let root = PathBuf::from(&root_str);
            if root.exists() && root.join(".git").exists() {
                self.project_root = Some(root.clone());
                self.current_cwd = Some(root.clone());
                self.git_root = Some(root);
                self.update_workspace_worktrees(cx);
                cx.emit(CwdChanged);
                cx.notify();
            }
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
        pending_project_root: None,
        switch_generation: 0,
        workspace: None,
        needs_restore: true,
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

            // Zerminal is terminal-first: always ensure a terminal
            // exists on startup, even if the pane has restored items.
            cx.defer_in(window, |workspace, window, cx| {
                let has_terminal = workspace
                    .active_pane()
                    .read(cx)
                    .items_of_type::<TerminalView>()
                    .next()
                    .is_some();
                if !has_terminal {
                    TerminalView::deploy(
                        workspace,
                        &NewCenterTerminal::default(),
                        window,
                        cx,
                    );
                }

                // Restore persisted project root so workspace context is
                // available immediately, without waiting for the user to
                // click a terminal tab. Spawned async to avoid reading
                // Workspace while it's being updated in this defer_in.
                let global = ActiveTerminalCwd::global(cx);
                cx.spawn(async move |_workspace, cx| {
                    global.update(cx, |this, cx| {
                        this.restore_project_root(cx);
                    });
                })
                .detach();
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
