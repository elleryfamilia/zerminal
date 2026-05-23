use gpui::{
    App, Context, Corner, DismissEvent, EventEmitter, FocusHandle, Focusable, Subscription, Window,
    WindowId,
};
use settings::{Settings, update_settings_file};
use theme::{GlobalTheme, WindowColorScheme, is_supported, window_color_schemes};
use theme_settings::ThemeSettings;
use ui::{PopoverMenu, Tooltip, prelude::*};

use crate::{AppState, MultiWorkspace, StatusItemView};

/// Status bar control (a paintbrush button, styled like the dock/AI buttons next
/// to it) that opens a list of per-project window color schemes growing upward
/// from the status bar.
pub struct WindowColorSelector;

impl WindowColorSelector {
    pub fn new() -> Self {
        Self
    }
}

impl Default for WindowColorSelector {
    fn default() -> Self {
        Self::new()
    }
}

impl Render for WindowColorSelector {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Only offer color schemes when the active theme supports them.
        if !is_supported(GlobalTheme::base_theme(cx)) {
            return div();
        }
        // Match the neighbouring dock/AI status bar buttons: muted on the status
        // bar accent, inverting to the accent background on hover.
        let hover_icon_color = cx
            .theme()
            .zerminal_status_bar_foreground
            .is_some()
            .then_some(Color::StatusBarBackground);
        // Small right padding so the brush sits the same visual distance from
        // its right-side neighbour as the AI icon does from the divider.
        div().pr(px(3.)).child(
            PopoverMenu::new("window-color-selector")
                .trigger(
                    IconButton::new("window-color-button", IconName::Paintbrush)
                        // Slightly smaller than the dock/AI buttons (20px) so the
                        // AI terminal icon stays the most prominent control.
                        .icon_size(IconSize::Custom(rems_from_px(16.)))
                        .icon_color(Color::StatusBarMuted)
                        .hover_icon_color(hover_icon_color)
                        .tooltip(Tooltip::text("Window Color")),
                )
                .anchor(Corner::BottomRight)
                .attach(Corner::TopRight)
                .menu(move |window, cx| Some(cx.new(|cx| WindowColorMenu::new(window, cx)))),
        )
    }
}

impl StatusItemView for WindowColorSelector {
    fn set_active_pane_item(
        &mut self,
        _active_pane_item: Option<&dyn crate::ItemHandle>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
    }
}

/// The popover list of color schemes. Hovering a row previews it on the window
/// live; clicking commits and persists it; dismissing without a click reverts to
/// the previously selected scheme.
struct WindowColorMenu {
    focus_handle: FocusHandle,
    window_id: WindowId,
    project_path: Option<String>,
    schemes: Vec<WindowColorScheme>,
    /// Scheme persisted for this project before the menu opened, restored on
    /// dismiss if the user doesn't commit a new choice.
    selected_key: Option<SharedString>,
    committed: bool,
    _revert_on_dismiss: Subscription,
}

impl EventEmitter<DismissEvent> for WindowColorMenu {}

impl Focusable for WindowColorMenu {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl WindowColorMenu {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let window_id = window.window_handle().window_id();
        let project_path = window
            .root::<MultiWorkspace>()
            .flatten()
            .and_then(|multi_workspace| {
                multi_workspace
                    .read(cx)
                    .workspace()
                    .read(cx)
                    .root_paths(cx)
                    .first()
                    .and_then(|path| path.to_str())
                    .map(|path| path.to_string())
            });

        let selected_key = project_path.as_ref().and_then(|path| {
            ThemeSettings::get_global(cx)
                .window_colors
                .get(path)
                .map(SharedString::from)
        });

        let revert_on_dismiss = cx.on_release(|this: &mut Self, cx: &mut App| {
            if !this.committed {
                GlobalTheme::set_window_color(cx, this.window_id, this.selected_key.clone());
                cx.refresh_windows();
            }
        });

        Self {
            focus_handle: cx.focus_handle(),
            window_id,
            project_path,
            schemes: window_color_schemes(),
            selected_key,
            committed: false,
            _revert_on_dismiss: revert_on_dismiss,
        }
    }

    fn commit(&mut self, key: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        self.committed = true;
        GlobalTheme::set_window_color(cx, self.window_id, Some(key.clone()));
        window.refresh();

        if let Some(path) = self.project_path.clone() {
            let fs = AppState::global(cx).fs.clone();
            update_settings_file(fs, cx, move |settings, _| {
                let zerminal = settings.zerminal.get_or_insert_with(Default::default);
                let window_colors = zerminal.window_colors.get_or_insert_with(Default::default);
                window_colors.insert(path, key.to_string());
            });
        }

        cx.emit(DismissEvent);
    }
}

impl Render for WindowColorMenu {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let hover_bg = cx.theme().colors().element_hover;
        let border = cx.theme().colors().border;
        let selected_key = self.selected_key.clone();

        v_flex()
            .key_context("WindowColorMenu")
            .track_focus(&self.focus_handle)
            .elevation_2(cx)
            .p_1()
            .min_w(px(232.))
            .children(self.schemes.iter().map(|scheme| {
                let key: SharedString = scheme.key.into();
                let label: SharedString = scheme.label.into();
                let background = scheme.background;
                let chrome = scheme.chrome;
                let is_selected = selected_key.as_deref() == Some(scheme.key);

                h_flex()
                    .id(scheme.key)
                    .w_full()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .cursor_pointer()
                    .hover(|style| style.bg(hover_bg))
                    .child(
                        // Larger preview (~2x): chrome bar over the window background.
                        div()
                            .w_12()
                            .h_7()
                            .rounded_md()
                            .overflow_hidden()
                            .border_1()
                            .border_color(border)
                            .bg(background)
                            .child(div().h(px(14.)).w_full().bg(chrome)),
                    )
                    .child(Label::new(label))
                    .child(div().flex_1())
                    .when(is_selected, |this| {
                        this.child(
                            Icon::new(IconName::Check)
                                .size(IconSize::Small)
                                .color(Color::Muted),
                        )
                    })
                    .on_hover(cx.listener({
                        let key = key.clone();
                        move |this, hovered: &bool, window, cx| {
                            if *hovered {
                                GlobalTheme::set_window_color(
                                    cx,
                                    this.window_id,
                                    Some(key.clone()),
                                );
                                window.refresh();
                            }
                        }
                    }))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.commit(key.clone(), window, cx)
                    }))
            }))
    }
}
