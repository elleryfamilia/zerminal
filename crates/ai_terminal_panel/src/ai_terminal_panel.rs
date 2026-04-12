use anyhow::Result;
use gpui::{
    Action, App, AsyncWindowContext, Context, Entity, EventEmitter, FocusHandle, Focusable,
    IntoElement, ParentElement, Pixels, Render, Styled, WeakEntity, Window, actions, px,
};
use terminal_view::TerminalView;
use ui::prelude::*;
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
                Box::new(workspace::NewTerminal { local: false }),
                false,
                window,
                cx,
            );
            pane.set_can_split(None);
            pane.set_can_navigate(false, cx);
            pane.set_render_tab_bar_buttons(cx, |_, _, _| (None, None));
            pane
        });

        Self {
            pane,
            workspace: workspace.weak_handle(),
            active: false,
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

    fn ensure_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.pane.read(cx).items_len() > 0 {
            return;
        }

        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };

        let project = workspace.read(cx).project().clone();
        let task = project.update(cx, |project, cx| {
            project.create_terminal_shell(None, cx)
        });
        let weak_workspace = self.workspace.clone();
        let weak_project = project.downgrade();

        cx.spawn_in(window, async move |this, cx| {
            let terminal = task.await?;
            this.update_in(cx, |this, window, cx| {
                let terminal_view = cx.new(|cx| {
                    TerminalView::new(
                        terminal,
                        weak_workspace,
                        None,
                        weak_project,
                        window,
                        cx,
                    )
                });
                this.pane.update(cx, |pane, cx| {
                    pane.add_item(Box::new(terminal_view), true, true, None, window, cx);
                });
            })
        })
        .detach_and_log_err(cx);
    }
}

impl EventEmitter<PanelEvent> for AiTerminalPanel {}

impl Render for AiTerminalPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .child(self.pane.clone())
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
        // Fixed to right dock for now
    }

    fn default_size(&self, _window: &Window, _cx: &App) -> Pixels {
        px(480.0)
    }

    fn icon(&self, _window: &Window, _cx: &App) -> Option<IconName> {
        Some(IconName::Sparkle)
    }

    fn icon_tooltip(&self, _window: &Window, _cx: &App) -> Option<&'static str> {
        Some("AI Agent Terminal")
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
        3
    }

    fn set_active(&mut self, active: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.active = active;
        if active {
            self.ensure_terminal(window, cx);
        }
    }

    fn starts_open(&self, _window: &Window, _cx: &App) -> bool {
        false
    }
}
