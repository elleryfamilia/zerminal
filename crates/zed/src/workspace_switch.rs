use std::path::PathBuf;

use active_terminal_cwd::{ActiveTerminalCwd, ProjectSwitchRequested};
use ai_terminal_panel::AiTerminalPanel;
use gpui::{App, Context, Entity, EntityId, PromptLevel, WeakEntity};
use terminal_view::TerminalView;
use workspace::notifications::NotificationId;
use workspace::{Panel as _, Toast, Workspace};
use workspace::pane::SaveIntent;

pub fn init(cx: &mut App) {
    cx.observe_new(
        |_workspace: &mut Workspace, window, cx: &mut Context<Workspace>| {
            let Some(window) = window else { return };
            let Some(global) = ActiveTerminalCwd::try_global(cx) else {
                return;
            };

            cx.subscribe_in(
                &global,
                window,
                |workspace, cwd_entity, event: &ProjectSwitchRequested, window, cx| {
                    if event.origin_workspace != cx.entity_id() {
                        return;
                    }
                    let new_root = event.new_root.clone();
                    let generation = cwd_entity.read(cx).switch_generation();
                    let global = cwd_entity.clone();

                    let has_dirty_items = workspace.items(cx).any(|item| item.is_dirty(cx));
                    let has_ai_session = has_active_ai_session(workspace, cx);
                    let current_project = current_project_name(workspace, cx);

                    if !has_dirty_items && !has_ai_session {
                        cx.spawn_in(window, async move |this, cx| {
                            execute_switch_and_cleanup(
                                &this, &global, new_root, generation, false, cx,
                            )
                        })
                        .detach_and_log_err(cx);
                        return;
                    }

                    cx.spawn_in(window, async move |this, cx| {
                        if has_dirty_items {
                            let new_project = project_display_name(&new_root);
                            let detail = format!(
                                "You have unsaved changes. Switch to {}?",
                                new_project,
                            );

                            let answer = cx.update(|window, cx| {
                                window.prompt(
                                    PromptLevel::Warning,
                                    "Unsaved changes",
                                    Some(&detail),
                                    &["Save & Switch", "Switch without saving"],
                                    cx,
                                )
                            })?;

                            if let Ok(0) = answer.await {
                                let save_task =
                                    this.update_in(cx, |workspace, window, cx| {
                                        workspace.save_all_dirty_items(window, cx)
                                    })?;
                                if let Err(err) = save_task.await {
                                    log::error!(
                                        "workspace_switch: save failed: {err}"
                                    );
                                }
                            }
                        }

                        if has_ai_session {
                            let new_project = project_display_name(&new_root);
                            let current = current_project.as_deref()
                                .unwrap_or("the current project");

                            let detail = format!(
                                "Your coding tool is working in {current}. \
                                 Switching to {new_project} will end the current session.",
                            );

                            let keep_label =
                                format!("Keep coding tool in {current}");

                            let answer = cx.update(|window, cx| {
                                window.prompt(
                                    PromptLevel::Warning,
                                    "Active coding tool session",
                                    Some(&detail),
                                    &["Switch & start new session", &keep_label],
                                    cx,
                                )
                            })?;

                            match answer.await {
                                Ok(0) => {
                                    // Close old AI session — cleanup will
                                    // handle it below via reset_ai flag
                                }
                                _ => {
                                    let warning = format!(
                                        "Your coding tool is working in {current} \
                                         while your terminal is in {new_project}",
                                    );
                                    this.update_in(cx, |workspace, _window, cx| {
                                        workspace.show_toast(
                                            Toast::new(
                                                NotificationId::unique::<AiTerminalPanel>(),
                                                warning,
                                            ),
                                            cx,
                                        );
                                    })?;

                                    cx.update(|_window, app| {
                                        global.update(app, |this, _cx| {
                                            this.cancel_worktree_switch(generation);
                                        });
                                    })?;
                                    return anyhow::Ok(());
                                }
                            }
                        }

                        execute_switch_and_cleanup(
                            &this, &global, new_root, generation, has_ai_session, cx,
                        )
                    })
                    .detach_and_log_err(cx);
                },
            )
            .detach();
        },
    )
    .detach();
}

fn has_active_ai_session(workspace: &Workspace, cx: &App) -> bool {
    workspace
        .panel::<AiTerminalPanel>(cx)
        .is_some_and(|panel| {
            let panel_id = Entity::entity_id(&panel);
            let is_visible = workspace.all_docks().iter().any(|dock| {
                dock.read(cx)
                    .visible_panel()
                    .is_some_and(|visible| visible.panel_id() == panel_id)
            });
            let has_items = panel
                .read(cx)
                .pane()
                .is_some_and(|pane| pane.read(cx).items_len() > 0);
            is_visible && has_items
        })
}

fn current_project_name(workspace: &Workspace, cx: &App) -> Option<String> {
    let project = workspace.project().read(cx);
    let worktree = project.visible_worktrees(cx).next()?;
    Some(worktree.read(cx).root_name_str().to_string())
}

fn execute_switch_and_cleanup(
    workspace: &WeakEntity<Workspace>,
    global: &Entity<ActiveTerminalCwd>,
    new_root: PathBuf,
    generation: u64,
    reset_ai_session: bool,
    cx: &mut gpui::AsyncWindowContext,
) -> anyhow::Result<()> {
    cx.update(|_window, app| {
        global.update(app, |this, cx| {
            this.execute_worktree_switch(new_root, generation, cx);
        });
    })?;

    workspace.update_in(cx, |workspace, window, cx| {
        // Close all non-terminal editor items
        let panes: Vec<_> = workspace.panes().to_vec();
        for pane in panes {
            let non_terminal_ids: Vec<EntityId> = pane
                .read(cx)
                .items()
                .filter(|item| {
                    item.to_any_view().downcast::<TerminalView>().is_err()
                })
                .map(|item| item.item_id())
                .collect();

            if !non_terminal_ids.is_empty() {
                pane.update(cx, |pane, cx| {
                    pane.close_items(window, cx, SaveIntent::Skip, &|id| {
                        non_terminal_ids.contains(&id)
                    })
                    .detach_and_log_err(cx);
                });
            }
        }

        // Close AI terminal sessions and reopen the panel so the user
        // can pick a new tool. Closing all pane items triggers
        // PanelEvent::Close, so we reopen immediately after.
        if reset_ai_session {
            if let Some(panel) = workspace.panel::<AiTerminalPanel>(cx) {
                if let Some(pane) = panel.read(cx).pane() {
                    pane.update(cx, |pane, cx| {
                        pane.close_items(window, cx, SaveIntent::Skip, &|_| true)
                            .detach_and_log_err(cx);
                    });
                }
            }
            workspace.open_panel::<AiTerminalPanel>(window, cx);
        }
    })?;

    anyhow::Ok(())
}

fn project_display_name(path: &PathBuf) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}
