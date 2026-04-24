use std::{num::NonZeroUsize, time::Duration};

use crate::DockPosition;
use collections::HashMap;
use gpui::{App, Hsla, Window, WindowBackgroundAppearance};
use serde::Deserialize;
pub use settings::{
    AutosaveSetting, BottomDockLayout, EncodingDisplayOptions, InactiveOpacity,
    PaneSplitDirectionHorizontal, PaneSplitDirectionVertical, RegisterSetting,
    RestoreOnStartupBehavior, Settings,
};
use theme::ActiveTheme;

#[derive(RegisterSetting)]
pub struct WorkspaceSettings {
    pub active_pane_modifiers: ActivePanelModifiers,
    pub bottom_dock_layout: settings::BottomDockLayout,
    pub pane_split_direction_horizontal: settings::PaneSplitDirectionHorizontal,
    pub pane_split_direction_vertical: settings::PaneSplitDirectionVertical,
    pub centered_layout: settings::CenteredLayoutSettings,
    pub confirm_quit: bool,
    pub show_call_status_icon: bool,
    pub autosave: AutosaveSetting,
    pub restore_on_startup: settings::RestoreOnStartupBehavior,
    pub cli_default_open_behavior: settings::CliDefaultOpenBehavior,
    pub restore_on_file_reopen: bool,
    pub drop_target_size: f32,
    pub use_system_path_prompts: bool,
    pub use_system_prompts: bool,
    pub command_aliases: HashMap<String, String>,
    pub max_tabs: Option<NonZeroUsize>,
    pub when_closing_with_no_tabs: settings::CloseWindowWhenNoItems,
    pub on_last_window_closed: settings::OnLastWindowClosed,
    pub text_rendering_mode: settings::TextRenderingMode,
    pub resize_all_panels_in_dock: Vec<DockPosition>,
    pub close_on_file_delete: bool,
    pub close_panel_on_toggle: bool,
    pub use_system_window_tabs: bool,
    pub zoomed_padding: bool,
    pub window_decorations: settings::WindowDecorations,
    pub focus_follows_mouse: FocusFollowsMouse,
    pub window: WindowSettings,
}

#[derive(Copy, Clone, PartialEq, Debug)]
pub struct WindowSettings {
    /// Clamped to [0.05, 1.0]. Below 1.0 makes chrome translucent.
    pub opacity: f32,
    /// Request OS-level backdrop blur. Degrades silently when the compositor can't.
    pub blur: bool,
}

impl Default for WindowSettings {
    fn default() -> Self {
        Self {
            opacity: 1.0,
            blur: false,
        }
    }
}

impl WindowSettings {
    /// True when chrome surfaces should actually paint translucently.
    ///
    /// Only `opacity < 1.0` triggers translucency — `blur = true` alone requests
    /// OS-level backdrop blur but does not by itself fade chrome. This matches
    /// user expectation that `opacity: 1.0` means "fully opaque, no see-through",
    /// even if `blur: true` is also set (blur will simply be hidden in that case).
    pub fn user_wants_translucent(&self) -> bool {
        self.opacity < 1.0
    }
}

/// Resolve the effective [`WindowBackgroundAppearance`] given user settings and theme.
///
/// User precedence:
/// * `blur = true`  → `Blurred`
/// * `opacity < 1.0` → `Transparent`
/// * otherwise (defaults) → fall back to `cx.theme().window_background_appearance()`
///
/// The fallback preserves behavior for themes that ship a non-Opaque appearance —
/// users who don't touch the window settings keep whatever the theme asked for.
pub fn resolve_window_appearance(cx: &App) -> WindowBackgroundAppearance {
    let settings = WorkspaceSettings::get_global(cx).window;
    if settings.blur {
        return WindowBackgroundAppearance::Blurred;
    }
    if settings.opacity < 1.0 {
        return WindowBackgroundAppearance::Transparent;
    }
    cx.theme().window_background_appearance()
}

/// Multiply the alpha of a chrome surface color by the window's effective opacity.
///
/// Returns `color` unchanged when the effective window appearance is Opaque. Otherwise
/// multiplies `color.a` by `window.opacity` and a small focus-dim factor when the
/// window is not the active one. `Hsla::opacity` already multiplies (see `gpui::Hsla`),
/// so themes that encode their own translucency compose correctly.
///
/// Call this from every chrome paint site (background fills, tab bars, status bars,
/// sidebar, title bar, terminal bounds fill, …). Do NOT call it from popovers, menus,
/// modals, or tooltips — those stay opaque regardless.
pub fn apply_window_tint(color: Hsla, window: &Window, cx: &App) -> Hsla {
    let settings = WorkspaceSettings::get_global(cx).window;
    if !settings.user_wants_translucent() {
        return color;
    }
    let focus_factor = if window.is_window_active() {
        1.0
    } else {
        0.92
    };
    color.opacity(settings.opacity * focus_factor)
}

/// For the main content area (terminal pane) — fully transparent when translucent
/// so nested layers don't stack multiplicatively and defeat the blur.
pub fn apply_window_surface_tint(color: Hsla, _window: &Window, cx: &App) -> Hsla {
    let settings = WorkspaceSettings::get_global(cx).window;
    if !settings.user_wants_translucent() {
        return color;
    }
    gpui::transparent_black()
}

/// For side / AI / dock panes that need a touch more contrast against the blur
/// so users can distinguish where the file browser ends and the terminal begins.
///
/// Returns the color with alpha = (user opacity + 0.05), giving each pane a single
/// semi-opaque layer of chrome tint above the OS backdrop.
pub fn apply_window_pane_tint(color: Hsla, window: &Window, cx: &App) -> Hsla {
    let settings = WorkspaceSettings::get_global(cx).window;
    if !settings.user_wants_translucent() {
        return color;
    }
    let effective = (settings.opacity + 0.05).min(1.0);
    let focus_factor = if window.is_window_active() {
        1.0
    } else {
        0.92
    };
    color.opacity(effective * focus_factor)
}

/// True when user transparency is active — use to thicken borders between panes
/// so section boundaries stay legible over the blurred backdrop.
pub fn window_is_translucent(cx: &App) -> bool {
    WorkspaceSettings::get_global(cx)
        .window
        .user_wants_translucent()
}

/// Border color for section dividers (between dock, sidebar, terminal pane, etc).
///
/// Under user-initiated transparency, returns an opaque, higher-contrast border
/// so users can tell where one pane ends and the next begins — a hairline
/// `colors().border` typically gets lost against a blurred wallpaper. Off by
/// default, returns the original color when transparency isn't active.
pub fn apply_window_border_tint(color: Hsla, cx: &App) -> Hsla {
    let settings = WorkspaceSettings::get_global(cx).window;
    if !settings.user_wants_translucent() {
        return color;
    }
    // Boost toward full opacity and bump lightness away from the surrounding tint
    // so the edge actually reads.
    let theme = cx.theme();
    theme.colors().border_variant.opacity(1.0).blend(color)
}

#[derive(Copy, Clone, Deserialize)]
pub struct FocusFollowsMouse {
    pub enabled: bool,
    pub debounce: Duration,
}

#[derive(Copy, Clone, PartialEq, Debug, Default)]
pub struct ActivePanelModifiers {
    /// Size of the border surrounding the active pane.
    /// When set to 0, the active pane doesn't have any border.
    /// The border is drawn inset.
    ///
    /// Default: `0.0`
    // TODO: make this not an option, it is never None
    pub border_size: Option<f32>,
    /// Opacity of inactive panels.
    /// When set to 1.0, the inactive panes have the same opacity as the active one.
    /// If set to 0, the inactive panes content will not be visible at all.
    /// Values are clamped to the [0.0, 1.0] range.
    ///
    /// Default: `1.0`
    // TODO: make this not an option, it is never None
    pub inactive_opacity: Option<InactiveOpacity>,
}

#[derive(Deserialize, RegisterSetting)]
pub struct TabBarSettings {
    pub show: bool,
    pub show_nav_history_buttons: bool,
    pub show_tab_bar_buttons: bool,
    pub show_pinned_tabs_in_separate_row: bool,
}

impl Settings for WorkspaceSettings {
    fn from_settings(content: &settings::SettingsContent) -> Self {
        let workspace = &content.workspace;
        Self {
            active_pane_modifiers: ActivePanelModifiers {
                border_size: Some(
                    workspace
                        .active_pane_modifiers
                        .unwrap()
                        .border_size
                        .unwrap(),
                ),
                inactive_opacity: Some(
                    workspace
                        .active_pane_modifiers
                        .unwrap()
                        .inactive_opacity
                        .unwrap(),
                ),
            },
            bottom_dock_layout: workspace.bottom_dock_layout.unwrap(),
            pane_split_direction_horizontal: workspace.pane_split_direction_horizontal.unwrap(),
            pane_split_direction_vertical: workspace.pane_split_direction_vertical.unwrap(),
            centered_layout: workspace.centered_layout.unwrap(),
            confirm_quit: workspace.confirm_quit.unwrap(),
            show_call_status_icon: workspace.show_call_status_icon.unwrap(),
            autosave: workspace.autosave.unwrap(),
            restore_on_startup: workspace.restore_on_startup.unwrap(),
            cli_default_open_behavior: workspace.cli_default_open_behavior.unwrap(),
            restore_on_file_reopen: workspace.restore_on_file_reopen.unwrap(),
            drop_target_size: workspace.drop_target_size.unwrap(),
            use_system_path_prompts: workspace.use_system_path_prompts.unwrap(),
            use_system_prompts: workspace.use_system_prompts.unwrap(),
            command_aliases: workspace.command_aliases.clone(),
            max_tabs: workspace.max_tabs,
            when_closing_with_no_tabs: workspace.when_closing_with_no_tabs.unwrap(),
            on_last_window_closed: workspace.on_last_window_closed.unwrap(),
            text_rendering_mode: workspace.text_rendering_mode.unwrap(),
            resize_all_panels_in_dock: workspace
                .resize_all_panels_in_dock
                .clone()
                .unwrap()
                .into_iter()
                .map(Into::into)
                .collect(),
            close_on_file_delete: workspace.close_on_file_delete.unwrap(),
            close_panel_on_toggle: workspace.close_panel_on_toggle.unwrap(),
            use_system_window_tabs: workspace.use_system_window_tabs.unwrap(),
            zoomed_padding: workspace.zoomed_padding.unwrap(),
            window_decorations: workspace.window_decorations.unwrap(),
            focus_follows_mouse: FocusFollowsMouse {
                enabled: workspace
                    .focus_follows_mouse
                    .unwrap()
                    .enabled
                    .unwrap_or(false),
                debounce: Duration::from_millis(
                    workspace
                        .focus_follows_mouse
                        .unwrap()
                        .debounce_ms
                        .unwrap_or(250),
                ),
            },
            window: match workspace.window {
                Some(w) => WindowSettings {
                    opacity: w.opacity.unwrap_or(1.0).clamp(0.05, 1.0),
                    blur: w.blur.unwrap_or(false),
                },
                None => WindowSettings::default(),
            },
        }
    }
}

impl Settings for TabBarSettings {
    fn from_settings(content: &settings::SettingsContent) -> Self {
        let tab_bar = content.tab_bar.clone().unwrap();
        TabBarSettings {
            show: tab_bar.show.unwrap(),
            show_nav_history_buttons: tab_bar.show_nav_history_buttons.unwrap(),
            show_tab_bar_buttons: tab_bar.show_tab_bar_buttons.unwrap(),
            show_pinned_tabs_in_separate_row: tab_bar.show_pinned_tabs_in_separate_row.unwrap(),
        }
    }
}

#[derive(Deserialize, RegisterSetting)]
pub struct StatusBarSettings {
    pub show: bool,
    pub show_active_file: bool,
    pub active_language_button: bool,
    pub cursor_position_button: bool,
    pub line_endings_button: bool,
    pub active_encoding_button: EncodingDisplayOptions,
}

impl Settings for StatusBarSettings {
    fn from_settings(content: &settings::SettingsContent) -> Self {
        let status_bar = content.status_bar.clone().unwrap();
        StatusBarSettings {
            show: status_bar.show.unwrap(),
            show_active_file: status_bar.show_active_file.unwrap(),
            active_language_button: status_bar.active_language_button.unwrap(),
            cursor_position_button: status_bar.cursor_position_button.unwrap(),
            line_endings_button: status_bar.line_endings_button.unwrap(),
            active_encoding_button: status_bar.active_encoding_button.unwrap(),
        }
    }
}
