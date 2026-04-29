mod agent_detection;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use active_terminal_cwd::ActiveTerminalCwd;
use agent_detection::{AiAgent, detect_agents};
use anyhow::Result;
use claude_code_ide::ClaudeCodeAttachment;
use collections::HashMap;
use db::kvp::KeyValueStore;
use editor_capabilities::WorkspaceEditorCapabilities;
use gpui::{
    Action, App, AsyncWindowContext, Context, Corner, Entity, EntityId, EventEmitter, FocusHandle,
    Focusable, IntoElement, ParentElement, Pixels, Render, Styled, Task, WeakEntity, Window,
    actions, px,
};
use icons::IconName;
use serde::{Deserialize, Serialize};
use task::{HideStrategy, RevealStrategy, RevealTarget, SpawnInTerminal, TaskId};
use terminal_view::TerminalView;
use ui::{ContextMenu, PopoverMenu, Tooltip, prelude::*};
use util::ResultExt;
use workspace::{
    Pane, PaneGroup, SplitDirection, ToggleZoom, Workspace, WorkspaceId,
    dock::{DockPosition, Panel, PanelEvent},
    pane,
};

const AI_TERMINAL_PANEL_KEY: &str = "AiTerminalPanel";

actions!(
    ai_terminal_panel,
    [
        /// Toggles the AI terminal panel.
        Toggle,
        /// Toggles focus on the AI terminal panel.
        ToggleFocus,
        /// Toggles tile layout (all tabs visible as columns) for the AI terminal panel.
        ToggleTileMode,
    ]
);

#[derive(Clone, Debug, Default, Deserialize)]
pub struct CustomAgentConfig {
    pub name: String,
    pub command: String,
    pub args: Option<Vec<String>>,
    pub icon: Option<String>,
}

#[derive(
    Copy, Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum LayoutMode {
    #[default]
    Tabbed,
    Tiled,
}

#[derive(Default, Serialize, Deserialize)]
struct SerializedAiTerminalPanel {
    #[serde(default)]
    zoomed: bool,
    #[serde(default)]
    tile_mode: LayoutMode,
}

pub fn init(cx: &mut App) {
    cx.observe_new(
        |workspace: &mut Workspace, _window, _: &mut Context<Workspace>| {
            workspace.register_action(|workspace, _: &ToggleFocus, window, cx| {
                workspace.toggle_panel_focus::<AiTerminalPanel>(window, cx);
            });
            workspace.register_action(|workspace, _: &Toggle, window, cx| {
                if !workspace.toggle_panel_focus::<AiTerminalPanel>(window, cx) {
                    workspace.close_panel::<AiTerminalPanel>(window, cx);
                }
            });
            workspace.register_action(|workspace, _: &ToggleTileMode, window, cx| {
                if let Some(panel) = workspace.panel::<AiTerminalPanel>(cx) {
                    panel.update(cx, |panel, cx| panel.toggle_tile_mode(window, cx));
                }
            });
        },
    )
    .detach();
}

pub struct AiTerminalPanel {
    center: PaneGroup,
    active_pane: Entity<Pane>,
    workspace: WeakEntity<Workspace>,
    workspace_id: Option<WorkspaceId>,
    project: Entity<project::Project>,
    active: bool,
    zoomed: bool,
    tile_mode: LayoutMode,
    detected_agents: Vec<AiAgent>,
    pending_serialization: Task<Option<()>>,
    /// Claude `/ide` attachments keyed by the terminal view's entity id.
    /// Dropping the entry unlinks the lockfile and tears down the WS server.
    claude_attachments: HashMap<EntityId, Entity<ClaudeCodeAttachment>>,
}

impl AiTerminalPanel {
    pub fn new(
        workspace: &Workspace,
        zoomed: bool,
        tile_mode: LayoutMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let detected_agents = detect_agents(&[]);
        let workspace_id = workspace.database_id();
        let project = workspace.project().clone();
        let workspace_handle = workspace.weak_handle();
        let initial_pane = Self::new_ai_pane(workspace_handle.clone(), project.clone(), window, cx);

        let this = Self {
            center: PaneGroup::new(initial_pane.clone()),
            active_pane: initial_pane.clone(),
            workspace: workspace_handle,
            workspace_id,
            project,
            active: false,
            zoomed,
            tile_mode,
            detected_agents,
            pending_serialization: Task::ready(None),
            claude_attachments: HashMap::default(),
        };
        this.subscribe_to_pane(&initial_pane, window, cx);
        this.refresh_toolbar_placement(cx);
        this
    }

    fn new_ai_pane(
        workspace_handle: WeakEntity<Workspace>,
        project: Entity<project::Project>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<Pane> {
        cx.new(|cx| {
            let mut pane = Pane::new(
                workspace_handle,
                project,
                Default::default(),
                None,
                Box::new(gpui::NoAction),
                false,
                window,
                cx,
            );
            pane.set_can_split(None);
            pane.set_can_navigate(false, cx);
            Self::clear_tab_bar_buttons(&mut pane, cx);
            pane
        })
    }

    fn clear_tab_bar_buttons(pane: &mut Pane, cx: &mut Context<Pane>) {
        pane.set_render_tab_bar_buttons(cx, |_, _, _| (None, None));
    }

    fn refresh_toolbar_placement(&self, cx: &mut Context<Self>) {
        let panes: Vec<Entity<Pane>> = self.center.panes().iter().map(|p| (*p).clone()).collect();
        let Some(rightmost) = panes.last().cloned() else {
            return;
        };
        for pane in panes.iter().filter(|p| **p != rightmost) {
            pane.update(cx, |pane, cx| Self::clear_tab_bar_buttons(pane, cx));
        }
        self.apply_tab_bar_buttons(&rightmost, cx);
    }

    fn subscribe_to_pane(
        &self,
        pane: &Entity<Pane>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.subscribe_in(pane, window, Self::handle_pane_event)
            .detach();
    }

    fn handle_pane_event(
        &mut self,
        pane: &Entity<Pane>,
        event: &pane::Event,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            pane::Event::Remove { focus_on_pane } => {
                let pane_count_before = self.center.panes().len();
                let _ = self.center.remove(pane, cx);
                if pane_count_before <= 1 {
                    cx.emit(PanelEvent::Close);
                } else {
                    if self.active_pane == *pane {
                        self.active_pane = focus_on_pane
                            .clone()
                            .or_else(|| self.center.panes().first().map(|p| (*p).clone()))
                            .unwrap_or_else(|| self.center.first_pane());
                    }
                    self.active_pane.focus_handle(cx).focus(window, cx);
                    self.refresh_toolbar_placement(cx);
                    cx.notify();
                }
            }
            pane::Event::ZoomIn => {
                for pane in self.center.panes() {
                    pane.update(cx, |pane, cx| pane.set_zoomed(true, cx));
                }
                cx.emit(PanelEvent::ZoomIn);
                cx.notify();
            }
            pane::Event::ZoomOut => {
                for pane in self.center.panes() {
                    pane.update(cx, |pane, cx| pane.set_zoomed(false, cx));
                }
                cx.emit(PanelEvent::ZoomOut);
                cx.notify();
            }
            pane::Event::Focus => {
                self.active_pane = pane.clone();
                cx.notify();
            }
            _ => {}
        }
    }

    fn apply_tab_bar_buttons(&self, target: &Entity<Pane>, cx: &mut Context<Self>) {
        let agents_for_menu: Arc<Vec<AiAgent>> = Arc::new(self.detected_agents.clone());
        let weak_panel = cx.entity().downgrade();

        target.update(cx, |pane, cx| {
            pane.set_render_tab_bar_buttons(cx, move |pane, _window, cx| {
                let agents = agents_for_menu.clone();
                let weak_panel = weak_panel.clone();
                let tile_mode = weak_panel
                    .upgrade()
                    .map(|panel| panel.read(cx).tile_mode)
                    .unwrap_or_default();
                let is_tiled = matches!(tile_mode, LayoutMode::Tiled);

                let is_zoomed = pane.is_zoomed();
                let zoom_button = IconButton::new("ai-panel-zoom", IconName::Maximize)
                    .icon_size(IconSize::Small)
                    .toggle_state(is_zoomed)
                    .selected_icon(IconName::Minimize)
                    .selected_icon_color(Color::Accent)
                    .tooltip(move |_, cx| {
                        Tooltip::for_action(
                            if is_zoomed {
                                "Disable Full Screen"
                            } else {
                                "Enable Full Screen"
                            },
                            &ToggleZoom,
                            cx,
                        )
                    })
                    .on_click(cx.listener(|pane, _, window, cx| {
                        pane.toggle_zoom(&ToggleZoom, window, cx);
                    }));

                let current_pane_id = cx.entity_id();
                let current_pane_items = pane.items_len();
                let total_items = weak_panel
                    .upgrade()
                    .map(|panel| {
                        panel
                            .read(cx)
                            .center
                            .panes()
                            .iter()
                            .map(|p| {
                                if p.entity_id() == current_pane_id {
                                    current_pane_items
                                } else {
                                    p.read(cx).items_len()
                                }
                            })
                            .sum::<usize>()
                    })
                    .unwrap_or(current_pane_items);
                let show_tile_button = total_items > 1;
                let tile_button = IconButton::new("ai-panel-tile", IconName::VerticalPanes)
                .icon_size(IconSize::Small)
                .toggle_state(is_tiled)
                .tooltip(move |_, cx| {
                    Tooltip::for_action(
                        if is_tiled {
                            "Show Tabs"
                        } else {
                            "Show All Tabs Side-by-Side"
                        },
                        &ToggleTileMode,
                        cx,
                    )
                })
                .on_click(|_, window, cx| {
                    window.dispatch_action(Box::new(ToggleTileMode), cx);
                });

                let menu = PopoverMenu::new("ai-agent-add-menu")
                    .trigger(
                        IconButton::new("add-agent", IconName::Plus)
                            .icon_size(IconSize::Small)
                            .tooltip(Tooltip::text("New Agent Tab")),
                    )
                    .anchor(Corner::TopRight)
                    .menu(move |_window, cx| {
                        let agents = agents.clone();
                        let weak_panel = weak_panel.clone();
                        Some(ContextMenu::build(_window, cx, move |mut menu, _, _| {
                            for agent in agents.iter() {
                                let agent_clone = agent.clone();
                                let weak = weak_panel.clone();
                                menu = menu.entry(
                                    agent.name.clone(),
                                    None,
                                    move |window, cx| {
                                        if let Some(panel) = weak.upgrade() {
                                            panel.update(cx, |panel, cx| {
                                                panel.spawn_agent(&agent_clone, window, cx);
                                            });
                                        }
                                    },
                                );
                            }
                            menu
                        }))
                    });

                let buttons = h_flex()
                    .gap_0p5()
                    .child(zoom_button)
                    .when(show_tile_button, |this| this.child(tile_button))
                    .child(menu)
                    .into_any_element();
                (None, Some(buttons))
            });
        });
    }

    pub fn load(
        workspace: WeakEntity<Workspace>,
        cx: &mut AsyncWindowContext,
    ) -> gpui::Task<Result<Entity<Self>>> {
        cx.spawn(async move |cx| {
            let key_and_store = workspace
                .read_with(cx, |workspace, cx| {
                    serialization_key(workspace.database_id())
                        .map(|key| (key, KeyValueStore::global(cx)))
                })
                .ok()
                .flatten();
            let serialized = if let Some((key, kvp)) = key_and_store {
                cx.background_spawn(async move { kvp.read_kvp(&key) })
                    .await
                    .log_err()
                    .flatten()
                    .and_then(|raw| {
                        serde_json::from_str::<SerializedAiTerminalPanel>(&raw).log_err()
                    })
            } else {
                None
            };

            let serialized = serialized.unwrap_or_default();
            workspace.update_in(cx, |workspace, window, cx| {
                cx.new(|cx| {
                    Self::new(
                        workspace,
                        serialized.zoomed,
                        serialized.tile_mode,
                        window,
                        cx,
                    )
                })
            })
        })
    }

    pub fn toggle_tile_mode(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.tile_mode {
            LayoutMode::Tabbed => self.enter_tile_mode(window, cx),
            LayoutMode::Tiled => self.leave_tile_mode(window, cx),
        }
    }

    fn enter_tile_mode(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let source_pane = self.active_pane.clone();
        let project = self.project.clone();
        let workspace_handle = self.workspace.clone();

        let items_after_first: Vec<Box<dyn workspace::ItemHandle>> =
            source_pane.update(cx, |pane, _cx| {
                pane.items()
                    .skip(1)
                    .map(|item| item.boxed_clone())
                    .collect()
            });

        if items_after_first.is_empty() {
            self.tile_mode = LayoutMode::Tiled;
            self.schedule_serialize(cx);
            cx.notify();
            return;
        }

        source_pane.update(cx, |pane, cx| {
            for item in &items_after_first {
                pane.remove_item(item.item_id(), false, false, window, cx);
            }
        });

        let mut previous = source_pane.clone();
        for item in items_after_first {
            let new_pane =
                Self::new_ai_pane(workspace_handle.clone(), project.clone(), window, cx);
            self.subscribe_to_pane(&new_pane, window, cx);
            new_pane.update(cx, |pane, cx| {
                pane.add_item(item, false, false, None, window, cx);
            });
            self.center
                .split(&previous, &new_pane, SplitDirection::Right, cx);
            previous = new_pane;
        }

        self.tile_mode = LayoutMode::Tiled;
        self.active_pane = source_pane;
        self.active_pane.focus_handle(cx).focus(window, cx);
        self.refresh_toolbar_placement(cx);
        self.schedule_serialize(cx);
        cx.notify();
    }

    fn leave_tile_mode(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let panes = self.center.panes().iter().map(|&p| p.clone()).collect::<Vec<_>>();
        if panes.len() <= 1 {
            self.tile_mode = LayoutMode::Tabbed;
            self.schedule_serialize(cx);
            cx.notify();
            return;
        }
        let destination = panes[0].clone();

        for pane in panes.iter().skip(1) {
            let items: Vec<Box<dyn workspace::ItemHandle>> = pane.update(cx, |pane, _cx| {
                pane.items().map(|item| item.boxed_clone()).collect()
            });
            pane.update(cx, |pane, cx| {
                for item in &items {
                    pane.remove_item(item.item_id(), false, false, window, cx);
                }
            });
            destination.update(cx, |dest, cx| {
                for item in items {
                    dest.add_item(item, false, false, None, window, cx);
                }
            });
            let _ = self.center.remove(pane, cx);
        }

        self.active_pane = destination;
        self.active_pane.focus_handle(cx).focus(window, cx);
        self.tile_mode = LayoutMode::Tabbed;
        self.refresh_toolbar_placement(cx);
        self.schedule_serialize(cx);
        cx.notify();
    }

    fn schedule_serialize(&mut self, cx: &mut Context<Self>) {
        let Some(workspace_id) = self.workspace_id else {
            return;
        };
        let zoomed = self.zoomed;
        let tile_mode = self.tile_mode;
        let kvp = KeyValueStore::global(cx);
        self.pending_serialization = cx.spawn(async move |_, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(50))
                .await;
            let key = serialization_key(Some(workspace_id))?;
            let payload = SerializedAiTerminalPanel { zoomed, tile_mode };
            let serialized = serde_json::to_string(&payload).log_err()?;
            kvp.write_kvp(key, serialized).await.log_err();
            Some(())
        });
    }

    fn spawn_agent(
        &mut self,
        agent: &AiAgent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };

        let cwd: Option<PathBuf> = ActiveTerminalCwd::for_workspace(workspace.entity_id(), cx)
            .and_then(|entity| entity.read(cx).current_cwd().map(|p| p.to_path_buf()));

        log::info!(
            "AiTerminalPanel::spawn_agent invoked for agent name={:?} command={:?}",
            agent.name,
            agent.command
        );
        let claude_attachment = if agent.command == "claude" {
            log::info!("Preparing Claude /ide attachment");
            self.prepare_claude_attachment(&workspace, &cwd, window, cx)
        } else {
            None
        };
        let env = claude_attachment
            .as_ref()
            .map(|(_, env)| env.clone())
            .unwrap_or_default();
        if !env.is_empty() {
            log::info!(
                "Claude /ide env to inject: keys={:?}",
                env.keys().collect::<Vec<_>>()
            );
        }

        let spawn_task = SpawnInTerminal {
            id: TaskId(format!("ai-agent-{}", agent.command)),
            full_label: agent.name.clone(),
            label: agent.name.clone(),
            command: Some(agent.path.to_string_lossy().to_string()),
            args: agent.args.clone(),
            command_label: agent.name.clone(),
            cwd,
            env,
            use_new_terminal: true,
            allow_concurrent_runs: true,
            reveal: RevealStrategy::Always,
            reveal_target: RevealTarget::Dock,
            hide: HideStrategy::Always,
            shell: Default::default(),
            show_summary: false,
            show_command: false,
            show_rerun: false,
            save: Default::default(),
        };

        let agent_icon = agent.icon;
        let project = workspace.read(cx).project().clone();
        let task = project.update(cx, |project, cx| {
            project.create_terminal_task(spawn_task, cx)
        });
        let weak_workspace = self.workspace.clone();
        let weak_project = project.downgrade();

        cx.spawn_in(window, async move |this, cx| {
            let terminal = task.await?;
            this.update_in(cx, |panel, window, cx| {
                let terminal_view = cx.new(|cx| {
                    let mut view = TerminalView::new(
                        terminal.clone(),
                        weak_workspace,
                        None,
                        weak_project,
                        window,
                        cx,
                    );
                    view.agent_icon = Some(agent_icon);
                    view
                });
                let tv_id = terminal_view.entity_id();
                let destination_pane = panel.destination_pane_for_spawn(window, cx);
                destination_pane.update(cx, |pane, cx| {
                    pane.add_item(Box::new(terminal_view), true, true, None, window, cx);
                });

                if let Some((attachment, _env)) = claude_attachment {
                    panel.claude_attachments.insert(tv_id, attachment);
                }

                let pane_for_close = destination_pane.clone();
                cx.subscribe_in(&terminal, window, move |this, _terminal, event: &terminal::Event, window, cx| {
                    if matches!(event, terminal::Event::CloseTerminal) {
                        this.claude_attachments.remove(&tv_id);
                        pane_for_close.update(cx, |pane, cx| {
                            pane.close_item_by_id(
                                tv_id,
                                workspace::SaveIntent::Close,
                                window,
                                cx,
                            ).detach_and_log_err(cx);
                        });
                    }
                }).detach();
            })
        })
        .detach_and_log_err(cx);
    }

    /// Build a [`ClaudeCodeAttachment`] for an upcoming `claude` spawn. Returns
    /// the entity (which the panel must hold) plus the env vars to inject so
    /// the CLI auto-connects to the WebSocket server. Returns `None` if any
    /// step fails — in which case `claude` is launched without integration
    /// (it still runs as a normal CLI in the pane).
    fn prepare_claude_attachment(
        &self,
        workspace: &Entity<Workspace>,
        cwd: &Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<(Entity<ClaudeCodeAttachment>, HashMap<String, String>)> {
        let workspace_root = cwd.clone().or_else(|| {
            workspace
                .read(cx)
                .visible_worktrees(cx)
                .next()
                .map(|worktree| worktree.read(cx).abs_path().to_path_buf())
        })?;

        let capabilities = Arc::new(WorkspaceEditorCapabilities::new(
            workspace.downgrade(),
            window.window_handle(),
        ));

        match ClaudeCodeAttachment::prepare(workspace_root.clone(), capabilities, cx) {
            Ok((entity, env)) => {
                let port = entity.read(cx).port();
                log::info!(
                    "Claude /ide attachment ready: port={port} workspace_root={:?}",
                    workspace_root.display()
                );
                Some((entity, env.into_iter().collect()))
            }
            Err(error) => {
                log::warn!("Failed to prepare Claude /ide attachment: {error:#}");
                None
            }
        }
    }

    fn destination_pane_for_spawn(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<Pane> {
        if !matches!(self.tile_mode, LayoutMode::Tiled) {
            return self.active_pane.clone();
        }
        let active_items = self.active_pane.read(cx).items_len();
        if active_items == 0 {
            return self.active_pane.clone();
        }
        let rightmost = self
            .center
            .panes()
            .last()
            .map(|p| (*p).clone())
            .unwrap_or_else(|| self.active_pane.clone());
        let new_pane = Self::new_ai_pane(
            self.workspace.clone(),
            self.project.clone(),
            window,
            cx,
        );
        self.subscribe_to_pane(&new_pane, window, cx);
        self.center
            .split(&rightmost, &new_pane, SplitDirection::Right, cx);
        self.active_pane = new_pane.clone();
        self.refresh_toolbar_placement(cx);
        new_pane
    }

    fn render_launcher(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let agents = self.detected_agents.clone();

        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_4()
            .child(
                Headline::new("Coding Tools")
                    .size(HeadlineSize::Small)
                    .color(Color::Muted),
            )
            .when(agents.is_empty(), |el| {
                el.child(
                    Label::new("No tools detected")
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                )
                .child(
                    Label::new("Install Claude Code, Codex, Aider, or others")
                        .size(LabelSize::XSmall)
                        .color(Color::Muted),
                )
            })
            .children(agents.into_iter().enumerate().map(|(ix, agent)| {
                let agent_clone = agent.clone();
                Button::new(("agent", ix), agent.name.clone())
                    .start_icon(Icon::new(agent.icon).size(IconSize::Medium))
                    .style(ButtonStyle::Outlined)
                    .size(ButtonSize::Large)
                    .full_width()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.spawn_agent(&agent_clone, window, cx);
                    }))
            }))
    }

    fn has_any_items(&self, cx: &App) -> bool {
        self.center
            .panes()
            .iter()
            .any(|pane| pane.read(cx).items_len() > 0)
    }
}

fn serialization_key(workspace_id: Option<WorkspaceId>) -> Option<String> {
    workspace_id.map(|id| format!("{}-{}", AI_TERMINAL_PANEL_KEY, i64::from(id)))
}

impl EventEmitter<PanelEvent> for AiTerminalPanel {}

impl Render for AiTerminalPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let has_items = self.has_any_items(cx);
        let bg = cx.theme().colors().panel_background;

        if !has_items {
            return div()
                .size_full()
                .bg(bg)
                .child(self.render_launcher(cx))
                .into_any_element();
        }

        let Some(workspace) = self.workspace.upgrade() else {
            return div().size_full().bg(bg).into_any_element();
        };
        workspace
            .update(cx, |workspace, cx| {
                let follower_states = HashMap::default();
                let weak_workspace = workspace.weak_handle();
                let ctx = workspace::PaneRenderContext {
                    follower_states: &follower_states,
                    active_call: workspace.active_call(),
                    active_pane: &self.active_pane,
                    app_state: workspace.app_state(),
                    project: workspace.project(),
                    workspace: &weak_workspace,
                };
                div()
                    .size_full()
                    .bg(bg)
                    .child(self.center.render(workspace.zoomed_item(), &ctx, window, cx))
                    .into_any_element()
            })
    }
}

impl Focusable for AiTerminalPanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.active_pane.focus_handle(cx)
    }
}

impl Panel for AiTerminalPanel {
    fn position(&self, _window: &Window, _cx: &App) -> DockPosition {
        DockPosition::Right
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
        px(480.0)
    }

    fn min_size(&self, _window: &Window, _cx: &App) -> Option<Pixels> {
        Some(px(280.0))
    }

    fn icon(&self, _window: &Window, _cx: &App) -> Option<IconName> {
        Some(IconName::Sparkle)
    }

    fn icon_tooltip(&self, _window: &Window, _cx: &App) -> Option<&'static str> {
        Some("Coding Tools")
    }

    fn toggle_action(&self) -> Box<dyn Action> {
        Box::new(Toggle)
    }

    fn persistent_name() -> &'static str {
        "AiTerminalPanel"
    }

    fn panel_key() -> &'static str {
        AI_TERMINAL_PANEL_KEY
    }

    fn pane(&self) -> Option<Entity<Pane>> {
        Some(self.active_pane.clone())
    }

    fn activation_priority(&self) -> u32 {
        4
    }

    fn set_active(&mut self, active: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.active = active;
        if !active && self.zoomed {
            self.set_zoomed(false, window, cx);
        }
    }

    fn starts_open(&self, _window: &Window, _cx: &App) -> bool {
        false
    }

    fn is_zoomed(&self, _window: &Window, _cx: &App) -> bool {
        self.zoomed
    }

    fn set_zoomed(&mut self, zoomed: bool, _window: &mut Window, cx: &mut Context<Self>) {
        if self.zoomed == zoomed {
            return;
        }
        self.zoomed = zoomed;
        for pane in self.center.panes() {
            pane.update(cx, |pane, cx| pane.set_zoomed(zoomed, cx));
        }
        self.schedule_serialize(cx);
        cx.notify();
    }
}
