mod agent_detection;

use std::path::PathBuf;
use std::sync::Arc;

use active_terminal_cwd::ActiveTerminalCwd;
use agent_detection::{AiAgent, detect_agents};
use anyhow::Result;
use gpui::{
    Action, App, AsyncWindowContext, Context, Corner, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement, IntoElement, ParentElement, Pixels, Render, Styled, WeakEntity, Window,
    actions, px,
};
use icons::IconName;
use serde::Deserialize;
use task::{HideStrategy, RevealStrategy, RevealTarget, SpawnInTerminal, TaskId};
use terminal_view::TerminalView;
use ui::{ContextMenu, PopoverMenu, Tooltip, prelude::*};
use workspace::{
    Pane, Workspace,
    dock::{DockPosition, Panel, PanelEvent},
};

const AI_TERMINAL_PANEL_KEY: &str = "AiTerminalPanel";

actions!(
    ai_terminal_panel,
    [
        /// Toggles the AI terminal panel.
        Toggle,
        /// Toggles focus on the AI terminal panel.
        ToggleFocus
    ]
);

#[derive(Clone, Debug, Default, Deserialize)]
pub struct CustomAgentConfig {
    pub name: String,
    pub command: String,
    pub args: Option<Vec<String>>,
    pub icon: Option<String>,
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
        },
    )
    .detach();
}

pub struct AiTerminalPanel {
    pane: Entity<Pane>,
    workspace: WeakEntity<Workspace>,
    active: bool,
    detected_agents: Vec<AiAgent>,
}

impl AiTerminalPanel {
    pub fn new(workspace: &Workspace, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let project = workspace.project();
        let pane = cx.new(|cx| {
            let mut pane = Pane::new(
                workspace.weak_handle(),
                project.clone(),
                Default::default(),
                None,
                Box::new(gpui::NoAction),
                false,
                window,
                cx,
            );
            pane.set_can_split(None);
            pane.set_can_navigate(false, cx);
            pane
        });

        // Subscribe to pane removal to close panel when empty
        cx.subscribe_in(&pane, window, |_this, _, event: &workspace::pane::Event, _window, cx| {
            if matches!(event, workspace::pane::Event::Remove { .. }) {
                cx.emit(PanelEvent::Close);
            }
        })
        .detach();

        let detected_agents = detect_agents(&[]);
        let agents_for_menu: Arc<Vec<AiAgent>> = Arc::new(detected_agents.clone());
        let weak_panel = cx.entity().downgrade();

        pane.update(cx, |pane, cx| {
            pane.set_render_tab_bar_buttons(cx, move |_pane, _window, cx| {
                let agents = agents_for_menu.clone();
                let weak_panel = weak_panel.clone();

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
                                                panel.spawn_agent(
                                                    &agent_clone, window, cx,
                                                );
                                            });
                                        }
                                    },
                                );
                            }
                            menu
                        }))
                    });

                (None, Some(menu.into_any_element()))
            });
        });

        Self {
            pane,
            workspace: workspace.weak_handle(),
            active: false,
            detected_agents,
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

    fn spawn_agent(
        &mut self,
        agent: &AiAgent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };

        let cwd: Option<PathBuf> = ActiveTerminalCwd::try_global(cx)
            .and_then(|entity| entity.read(cx).current_cwd().map(|p| p.to_path_buf()));

        let spawn_task = SpawnInTerminal {
            id: TaskId(format!("ai-agent-{}", agent.command)),
            full_label: agent.name.clone(),
            label: agent.name.clone(),
            command: Some(agent.path.to_string_lossy().to_string()),
            args: agent.args.clone(),
            command_label: agent.name.clone(),
            cwd,
            env: Default::default(),
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
        let pane = self.pane.clone();

        cx.spawn_in(window, async move |_this, cx| {
            let terminal = task.await?;
            _this.update_in(cx, |_this, window, cx| {
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
                pane.update(cx, |pane, cx| {
                    pane.add_item(Box::new(terminal_view), true, true, None, window, cx);
                });

                // Watch for terminal exit and close the tab
                let pane_for_close = pane.clone();
                cx.subscribe_in(&terminal, window, move |_this, _terminal, event: &terminal::Event, window, cx| {
                    if matches!(event, terminal::Event::CloseTerminal) {
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
}

impl EventEmitter<PanelEvent> for AiTerminalPanel {}

impl Render for AiTerminalPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let has_items = self.pane.read(cx).items_len() > 0;

        div()
            .size_full()
            .bg(cx.theme().colors().panel_background)
            .when(has_items, |el| el.child(self.pane.clone()))
            .when(!has_items, |el| el.child(self.render_launcher(cx)))
    }
}

impl Focusable for AiTerminalPanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.pane.focus_handle(cx)
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
        Some(self.pane.clone())
    }

    fn activation_priority(&self) -> u32 {
        4
    }

    fn set_active(&mut self, active: bool, _window: &mut Window, _cx: &mut Context<Self>) {
        self.active = active;
    }

    fn starts_open(&self, _window: &Window, _cx: &App) -> bool {
        false
    }
}
