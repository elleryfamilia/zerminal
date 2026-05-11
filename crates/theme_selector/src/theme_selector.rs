mod icon_theme_selector;

use fs::Fs;
use fuzzy::{StringMatch, StringMatchCandidate, match_strings};
use gpui::{
    App, Context, DismissEvent, Entity, EventEmitter, Focusable, PromptLevel, Render, UpdateGlobal,
    WeakEntity, Window, actions,
};
use picker::{Picker, PickerDelegate};
use settings::{
    FontFamilyName, FontSize, PairedThemeFontSnapshot, Settings, SettingsStore,
    ZerminalSettingsContent, update_settings_file,
};
use std::sync::Arc;
use theme::{Appearance, SystemAppearance, Theme, ThemeMeta, ThemeRegistry, ZerminalThemeFonts};
use theme_settings::{
    ThemeAppearanceMode, ThemeName, ThemeSelection, ThemeSettings, appearance_to_mode,
};
use ui::{ListItem, ListItemSpacing, prelude::*, v_flex};
use util::ResultExt;
use workspace::{ModalView, Workspace, ui::HighlightedLabel, with_active_or_new_workspace};
use zed_actions::{ExtensionCategoryFilter, Extensions};

use crate::icon_theme_selector::{IconThemeSelector, IconThemeSelectorDelegate};

actions!(
    theme_selector,
    [
        /// Reloads all themes from disk.
        Reload
    ]
);

pub fn init(cx: &mut App) {
    cx.on_action(|action: &zed_actions::theme_selector::Toggle, cx| {
        let action = action.clone();
        with_active_or_new_workspace(cx, move |workspace, window, cx| {
            toggle_theme_selector(workspace, &action, window, cx);
        });
    });
    cx.on_action(|action: &zed_actions::icon_theme_selector::Toggle, cx| {
        let action = action.clone();
        with_active_or_new_workspace(cx, move |workspace, window, cx| {
            toggle_icon_theme_selector(workspace, &action, window, cx);
        });
    });
}

fn toggle_theme_selector(
    workspace: &mut Workspace,
    toggle: &zed_actions::theme_selector::Toggle,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let fs = workspace.app_state().fs.clone();
    workspace.toggle_modal(window, cx, |window, cx| {
        let delegate = ThemeSelectorDelegate::new(
            cx.entity().downgrade(),
            fs,
            toggle.themes_filter.as_ref(),
            cx,
        );
        ThemeSelector::new(delegate, window, cx)
    });
}

fn toggle_icon_theme_selector(
    workspace: &mut Workspace,
    toggle: &zed_actions::icon_theme_selector::Toggle,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let fs = workspace.app_state().fs.clone();
    workspace.toggle_modal(window, cx, |window, cx| {
        let delegate = IconThemeSelectorDelegate::new(
            cx.entity().downgrade(),
            fs,
            toggle.themes_filter.as_ref(),
            cx,
        );
        IconThemeSelector::new(delegate, window, cx)
    });
}

impl ModalView for ThemeSelector {}

struct ThemeSelector {
    picker: Entity<Picker<ThemeSelectorDelegate>>,
}

impl EventEmitter<DismissEvent> for ThemeSelector {}

impl Focusable for ThemeSelector {
    fn focus_handle(&self, cx: &App) -> gpui::FocusHandle {
        self.picker.focus_handle(cx)
    }
}

impl Render for ThemeSelector {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .key_context("ThemeSelector")
            .w(rems(34.))
            .child(self.picker.clone())
    }
}

impl ThemeSelector {
    pub fn new(
        delegate: ThemeSelectorDelegate,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let picker = cx.new(|cx| Picker::uniform_list(delegate, window, cx));
        Self { picker }
    }
}

struct ThemeSelectorDelegate {
    fs: Arc<dyn Fs>,
    themes: Vec<ThemeMeta>,
    matches: Vec<StringMatch>,
    /// The theme that was selected before the `ThemeSelector` menu was opened.
    ///
    /// We use this to return back to theme that was set if the user dismisses the menu.
    original_theme_settings: ThemeSettings,
    /// The current system appearance.
    original_system_appearance: Appearance,
    /// The currently selected new theme.
    new_theme: Arc<Theme>,
    selection_completed: bool,
    selected_theme: Option<Arc<Theme>>,
    selected_index: usize,
    selector: WeakEntity<ThemeSelector>,
}

impl ThemeSelectorDelegate {
    fn new(
        selector: WeakEntity<ThemeSelector>,
        fs: Arc<dyn Fs>,
        themes_filter: Option<&Vec<String>>,
        cx: &mut Context<ThemeSelector>,
    ) -> Self {
        let original_theme = cx.theme().clone();
        let original_theme_settings = ThemeSettings::get_global(cx).clone();
        let original_system_appearance = SystemAppearance::global(cx).0;

        let registry = ThemeRegistry::global(cx);
        let mut themes = registry
            .list()
            .into_iter()
            .filter(|meta| {
                if let Some(theme_filter) = themes_filter {
                    theme_filter.contains(&meta.name.to_string())
                } else {
                    true
                }
            })
            .collect::<Vec<_>>();

        // Sort by dark vs light, then by name.
        themes.sort_unstable_by(|a, b| {
            a.appearance
                .is_light()
                .cmp(&b.appearance.is_light())
                .then(a.name.cmp(&b.name))
        });

        let matches: Vec<StringMatch> = themes
            .iter()
            .map(|meta| StringMatch {
                candidate_id: 0,
                score: 0.0,
                positions: Default::default(),
                string: meta.name.to_string(),
            })
            .collect();

        // The current theme is likely in this list, so default to first showing that.
        let selected_index = matches
            .iter()
            .position(|mat| mat.string == original_theme.name)
            .unwrap_or(0);

        Self {
            fs,
            themes,
            matches,
            original_theme_settings,
            original_system_appearance,
            new_theme: original_theme, // Start with the original theme.
            selected_index,
            selection_completed: false,
            selected_theme: None,
            selector,
        }
    }

    fn show_selected_theme(
        &mut self,
        cx: &mut Context<Picker<ThemeSelectorDelegate>>,
    ) -> Option<Arc<Theme>> {
        if let Some(mat) = self.matches.get(self.selected_index) {
            let registry = ThemeRegistry::global(cx);

            match registry.get(&mat.string) {
                Ok(theme) => {
                    self.set_theme(theme.clone(), cx);
                    Some(theme)
                }
                Err(error) => {
                    log::error!("error loading theme {}: {}", mat.string, error);
                    None
                }
            }
        } else {
            None
        }
    }

    fn set_theme(&mut self, new_theme: Arc<Theme>, cx: &mut App) {
        // Update the global (in-memory) theme settings.
        SettingsStore::update_global(cx, |store, _| {
            override_global_theme(
                store,
                &new_theme,
                &self.original_theme_settings.theme,
                self.original_system_appearance,
            )
        });

        self.new_theme = new_theme;
    }
}

/// Overrides the global (in-memory) theme settings.
///
/// Note that this does **not** update the user's `settings.json` file (see the
/// [`ThemeSelectorDelegate::confirm`] method and [`theme_settings::set_theme`] function).
fn override_global_theme(
    store: &mut SettingsStore,
    new_theme: &Theme,
    original_theme: &ThemeSelection,
    system_appearance: Appearance,
) {
    let theme_name = ThemeName(new_theme.name.clone().into());
    let new_appearance = new_theme.appearance();
    let new_theme_is_light = new_appearance.is_light();

    let mut curr_theme_settings = store.get::<ThemeSettings>(None).clone();

    match (original_theme, &curr_theme_settings.theme) {
        // Override the currently selected static theme.
        (ThemeSelection::Static(_), ThemeSelection::Static(_)) => {
            curr_theme_settings.theme = ThemeSelection::Static(theme_name);
        }

        // If the current theme selection is dynamic, then only override the global setting for the
        // specific mode (light or dark).
        (
            ThemeSelection::Dynamic {
                mode: original_mode,
                light: original_light,
                dark: original_dark,
            },
            ThemeSelection::Dynamic { .. },
        ) => {
            let new_mode = update_mode_if_new_appearance_is_different_from_system(
                original_mode,
                system_appearance,
                new_appearance,
            );

            let updated_theme = retain_original_opposing_theme(
                new_theme_is_light,
                new_mode,
                theme_name,
                original_light,
                original_dark,
            );

            curr_theme_settings.theme = updated_theme;
        }

        // The theme selection mode changed while selecting new themes (someone edited the settings
        // file on disk while we had the dialogue open), so don't do anything.
        _ => return,
    };

    store.override_global(curr_theme_settings);
}

/// Helper function for determining the new [`ThemeAppearanceMode`] for the new theme.
///
/// If the the original theme mode was [`System`] and the new theme's appearance matches the system
/// appearance, we don't need to change the mode setting.
///
/// Otherwise, we need to change the mode in order to see the new theme.
///
/// [`System`]: ThemeAppearanceMode::System
fn update_mode_if_new_appearance_is_different_from_system(
    original_mode: &ThemeAppearanceMode,
    system_appearance: Appearance,
    new_appearance: Appearance,
) -> ThemeAppearanceMode {
    if original_mode == &ThemeAppearanceMode::System && system_appearance == new_appearance {
        ThemeAppearanceMode::System
    } else {
        appearance_to_mode(new_appearance)
    }
}

/// Helper function for updating / displaying the [`ThemeSelection`] while using the theme selector.
///
/// We want to retain the alternate theme selection of the original settings (before the menu was
/// opened), not the currently selected theme (which likely has changed multiple times while the
/// menu has been open).
fn retain_original_opposing_theme(
    new_theme_is_light: bool,
    new_mode: ThemeAppearanceMode,
    theme_name: ThemeName,
    original_light: &ThemeName,
    original_dark: &ThemeName,
) -> ThemeSelection {
    if new_theme_is_light {
        ThemeSelection::Dynamic {
            mode: new_mode,
            light: theme_name,
            dark: original_dark.clone(),
        }
    } else {
        ThemeSelection::Dynamic {
            mode: new_mode,
            light: original_light.clone(),
            dark: theme_name,
        }
    }
}

impl PickerDelegate for ThemeSelectorDelegate {
    type ListItem = ui::ListItem;

    fn placeholder_text(&self, _window: &mut Window, _cx: &mut App) -> Arc<str> {
        "Select Theme...".into()
    }

    fn match_count(&self) -> usize {
        self.matches.len()
    }

    fn confirm(
        &mut self,
        _secondary: bool,
        window: &mut Window,
        cx: &mut Context<Picker<ThemeSelectorDelegate>>,
    ) {
        let theme_name: Arc<str> = self.new_theme.name.as_str().into();
        let theme_appearance = self.new_theme.appearance;
        let system_appearance = SystemAppearance::global(cx).0;
        let paired_fonts = self.new_theme.zerminal_fonts.clone();

        telemetry::event!("Settings Changed", setting = "theme", value = theme_name);

        let Some(fonts) = paired_fonts else {
            self.selection_completed = true;
            let fs = self.fs.clone();
            update_settings_file(fs, cx, move |settings, _| {
                theme_settings::set_theme(
                    settings,
                    theme_name.clone(),
                    theme_appearance,
                    system_appearance,
                );
                restore_paired_theme_fonts_if_any(settings);
            });
            self.selector
                .update(cx, |_, cx| {
                    cx.emit(DismissEvent);
                })
                .ok();
            return;
        };

        let display_name = self.new_theme.name.clone();
        let detail = paired_fonts_prompt_detail(&fonts, &display_name);
        let prompt = window.prompt(
            PromptLevel::Info,
            &format!("Apply paired fonts for {display_name}?"),
            Some(&detail),
            &["Apply", "Cancel"],
            cx,
        );

        let fs = self.fs.clone();
        let selector = self.selector.clone();
        let enable_screensaver = display_name.starts_with("Type Shii");
        cx.spawn(async move |this, cx| {
            let response = prompt.await.ok();
            if response == Some(0) {
                this.update(cx, |this, _| {
                    this.delegate.selection_completed = true;
                })
                .ok();
                cx.update(|cx| {
                    update_settings_file(fs, cx, move |settings, _| {
                        theme_settings::set_theme(
                            settings,
                            theme_name.clone(),
                            theme_appearance,
                            system_appearance,
                        );
                        apply_paired_theme_fonts(settings, &fonts);
                        if enable_screensaver {
                            enable_terminal_screensaver(settings);
                        }
                    });
                });
            }
            selector
                .update(cx, |_, cx| cx.emit(DismissEvent))
                .log_err();
        })
        .detach();
    }

    fn dismissed(&mut self, _: &mut Window, cx: &mut Context<Picker<ThemeSelectorDelegate>>) {
        if !self.selection_completed {
            SettingsStore::update_global(cx, |store, _| {
                store.override_global(self.original_theme_settings.clone());
            });
            self.selection_completed = true;
        }

        self.selector
            .update(cx, |_, cx| cx.emit(DismissEvent))
            .log_err();
    }

    fn selected_index(&self) -> usize {
        self.selected_index
    }

    fn set_selected_index(
        &mut self,
        ix: usize,
        _: &mut Window,
        cx: &mut Context<Picker<ThemeSelectorDelegate>>,
    ) {
        self.selected_index = ix;
        self.selected_theme = self.show_selected_theme(cx);
    }

    fn update_matches(
        &mut self,
        query: String,
        window: &mut Window,
        cx: &mut Context<Picker<ThemeSelectorDelegate>>,
    ) -> gpui::Task<()> {
        let background = cx.background_executor().clone();
        let candidates = self
            .themes
            .iter()
            .enumerate()
            .map(|(id, meta)| StringMatchCandidate::new(id, &meta.name))
            .collect::<Vec<_>>();

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

            this.update(cx, |this, cx| {
                this.delegate.matches = matches;
                if query.is_empty() && this.delegate.selected_theme.is_none() {
                    this.delegate.selected_index = this
                        .delegate
                        .selected_index
                        .min(this.delegate.matches.len().saturating_sub(1));
                } else if let Some(selected) = this.delegate.selected_theme.as_ref() {
                    this.delegate.selected_index = this
                        .delegate
                        .matches
                        .iter()
                        .enumerate()
                        .find(|(_, mtch)| mtch.string == selected.name)
                        .map(|(ix, _)| ix)
                        .unwrap_or_default();
                } else {
                    this.delegate.selected_index = 0;
                }
                // Preserve the previously selected theme when the filter yields no results.
                if let Some(theme) = this.delegate.show_selected_theme(cx) {
                    this.delegate.selected_theme = Some(theme);
                }
            })
            .log_err();
        })
    }

    fn render_match(
        &self,
        ix: usize,
        selected: bool,
        _window: &mut Window,
        _cx: &mut Context<Picker<Self>>,
    ) -> Option<Self::ListItem> {
        let theme_match = &self.matches.get(ix)?;

        Some(
            ListItem::new(ix)
                .inset(true)
                .spacing(ListItemSpacing::Sparse)
                .toggle_state(selected)
                .child(HighlightedLabel::new(
                    theme_match.string.clone(),
                    theme_match.positions.clone(),
                )),
        )
    }

    fn render_footer(
        &self,
        _: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> Option<gpui::AnyElement> {
        Some(
            h_flex()
                .p_2()
                .w_full()
                .justify_between()
                .gap_2()
                .border_t_1()
                .border_color(cx.theme().colors().border_variant)
                .child(
                    Button::new("docs", "View Theme Docs")
                        .end_icon(
                            Icon::new(IconName::ArrowUpRight)
                                .size(IconSize::Small)
                                .color(Color::Muted),
                        )
                        .on_click(cx.listener(|_, _, _, cx| {
                            cx.open_url("https://zed.dev/docs/themes");
                        })),
                )
                .child(
                    Button::new("more-themes", "Install Themes").on_click(cx.listener({
                        move |_, _, window, cx| {
                            window.dispatch_action(
                                Box::new(Extensions {
                                    category_filter: Some(ExtensionCategoryFilter::Themes),
                                    id: None,
                                }),
                                cx,
                            );
                        }
                    })),
                )
                .into_any_element(),
        )
    }
}

fn paired_fonts_prompt_detail(fonts: &ZerminalThemeFonts, theme_name: &SharedString) -> String {
    fn line(label: &str, family: Option<&SharedString>, size: Option<f32>) -> Option<String> {
        match (family, size) {
            (Some(family), Some(size)) => Some(format!("  • {label}: {family} at {size:.0}px")),
            (Some(family), None) => Some(format!("  • {label}: {family}")),
            (None, Some(size)) => Some(format!("  • {label}: {size:.0}px")),
            (None, None) => None,
        }
    }

    let mut lines = vec![format!(
        "{theme_name} ships with paired fonts. Applying will overwrite the listed font settings:",
    )];
    if let Some(text) = line(
        "UI font",
        fonts.ui_font_family.as_ref(),
        fonts.ui_font_size,
    ) {
        lines.push(text);
    }
    if let Some(text) = line(
        "Editor font",
        fonts.buffer_font_family.as_ref(),
        fonts.buffer_font_size,
    ) {
        lines.push(text);
    }
    if let Some(text) = line(
        "Terminal font",
        fonts.terminal_font_family.as_ref(),
        fonts.terminal_font_size,
    ) {
        lines.push(text);
    }
    if theme_name.starts_with("Type Shii") {
        lines.push(String::new());
        lines.push(
            "Type Shii also ships an ambient particle screensaver that drifts behind idle terminal output. Applying will enable it; other themes never show it.".to_string(),
        );
    }
    lines.push(String::new());
    lines.push(
        "Your previous fonts will be remembered and restored when you switch to a theme without paired fonts."
            .to_string(),
    );
    lines.join("\n")
}

fn current_terminal_font_family(settings: &settings::SettingsContent) -> Option<String> {
    settings
        .terminal
        .as_ref()
        .and_then(|t| t.font_family.as_ref())
        .map(|f| f.0.to_string())
}

fn set_terminal_font_family(settings: &mut settings::SettingsContent, value: Option<String>) {
    if value.is_none() && settings.terminal.is_none() {
        return;
    }
    let terminal = settings.terminal.get_or_insert_with(Default::default);
    terminal.font_family = value.map(|v| FontFamilyName(v.into()));
}

fn current_buffer_font_family(settings: &settings::SettingsContent) -> Option<String> {
    settings
        .theme
        .buffer_font_family
        .as_ref()
        .map(|f| f.0.to_string())
}

fn current_ui_font_family(settings: &settings::SettingsContent) -> Option<String> {
    settings
        .theme
        .ui_font_family
        .as_ref()
        .map(|f| f.0.to_string())
}

fn current_terminal_font_size(settings: &settings::SettingsContent) -> Option<f32> {
    settings.terminal.as_ref().and_then(|t| t.font_size).map(|s| s.0)
}

fn set_terminal_font_size(settings: &mut settings::SettingsContent, value: Option<f32>) {
    if value.is_none() && settings.terminal.is_none() {
        return;
    }
    let terminal = settings.terminal.get_or_insert_with(Default::default);
    terminal.font_size = value.map(FontSize);
}

/// Writes a paired theme's font families to the user's settings, capturing a
/// snapshot of the previous fonts so that restoration is later possible.
///
/// When a snapshot already exists (paired→paired switch), the original prior
/// values are preserved and only the `applied_*` fields are updated for slots
/// that the new pairing touches. Slots the new pairing does not touch are
/// left untouched in both settings and snapshot.
/// Enable the ambient particle screensaver. Only invoked from the
/// theme-selector "Apply" path when switching to a Type Shii variant.
/// Leaves other screensaver settings (idle_seconds, theme, opacity, …) at
/// their defaults so the user can tune them later.
fn enable_terminal_screensaver(settings: &mut settings::SettingsContent) {
    let terminal = settings.terminal.get_or_insert_with(Default::default);
    let screensaver = terminal.screensaver.get_or_insert_with(Default::default);
    screensaver.enabled = Some(true);
}

fn apply_paired_theme_fonts(
    settings: &mut settings::SettingsContent,
    fonts: &ZerminalThemeFonts,
) {
    let zerminal = settings.zerminal.get_or_insert_with(Default::default);
    let mut snapshot = zerminal
        .paired_theme_font_snapshot
        .take()
        .unwrap_or_else(|| PairedThemeFontSnapshot {
            prior_ui_font_family: settings
                .theme
                .ui_font_family
                .as_ref()
                .map(|f| f.0.to_string()),
            prior_buffer_font_family: settings
                .theme
                .buffer_font_family
                .as_ref()
                .map(|f| f.0.to_string()),
            prior_terminal_font_family: current_terminal_font_family(settings),
            applied_ui_font_family: None,
            applied_buffer_font_family: None,
            applied_terminal_font_family: None,
            prior_ui_font_size: settings.theme.ui_font_size.map(|s| s.0),
            prior_buffer_font_size: settings.theme.buffer_font_size.map(|s| s.0),
            prior_terminal_font_size: current_terminal_font_size(settings),
            applied_ui_font_size: None,
            applied_buffer_font_size: None,
            applied_terminal_font_size: None,
        });

    if let Some(family) = fonts.ui_font_family.as_ref() {
        let value = family.to_string();
        settings.theme.ui_font_family = Some(FontFamilyName(value.clone().into()));
        snapshot.applied_ui_font_family = Some(value);
    }
    if let Some(family) = fonts.buffer_font_family.as_ref() {
        let value = family.to_string();
        settings.theme.buffer_font_family = Some(FontFamilyName(value.clone().into()));
        snapshot.applied_buffer_font_family = Some(value);
    }
    if let Some(family) = fonts.terminal_font_family.as_ref() {
        let value = family.to_string();
        set_terminal_font_family(settings, Some(value.clone()));
        snapshot.applied_terminal_font_family = Some(value);
    }
    if let Some(size) = fonts.ui_font_size {
        settings.theme.ui_font_size = Some(FontSize(size));
        snapshot.applied_ui_font_size = Some(size);
    }
    if let Some(size) = fonts.buffer_font_size {
        settings.theme.buffer_font_size = Some(FontSize(size));
        snapshot.applied_buffer_font_size = Some(size);
    }
    if let Some(size) = fonts.terminal_font_size {
        set_terminal_font_size(settings, Some(size));
        snapshot.applied_terminal_font_size = Some(size);
    }

    settings
        .zerminal
        .get_or_insert_with(Default::default)
        .paired_theme_font_snapshot = Some(snapshot);
}

/// Restores fonts previously captured by [`apply_paired_theme_fonts`].
///
/// For each slot the active pairing wrote, restore to the prior value only if
/// the current settings still match what was applied — that way a font the
/// user manually tweaked while the paired theme was active is preserved. The
/// snapshot is cleared once any restoration runs, regardless of whether
/// individual slots were actually rewritten.
fn restore_paired_theme_fonts_if_any(settings: &mut settings::SettingsContent) {
    let snapshot = match settings.zerminal.as_mut() {
        Some(zerminal) => match zerminal.paired_theme_font_snapshot.take() {
            Some(snapshot) => snapshot,
            None => return,
        },
        None => return,
    };

    if let Some(applied) = snapshot.applied_ui_font_family.as_ref() {
        if current_ui_font_family(settings).as_deref() == Some(applied.as_str()) {
            settings.theme.ui_font_family = snapshot
                .prior_ui_font_family
                .clone()
                .map(|v| FontFamilyName(v.into()));
        }
    }
    if let Some(applied) = snapshot.applied_buffer_font_family.as_ref() {
        if current_buffer_font_family(settings).as_deref() == Some(applied.as_str()) {
            settings.theme.buffer_font_family = snapshot
                .prior_buffer_font_family
                .clone()
                .map(|v| FontFamilyName(v.into()));
        }
    }
    if let Some(applied) = snapshot.applied_terminal_font_family.as_ref() {
        if current_terminal_font_family(settings).as_deref() == Some(applied.as_str()) {
            set_terminal_font_family(settings, snapshot.prior_terminal_font_family.clone());
        }
    }
    if let Some(applied) = snapshot.applied_ui_font_size {
        if settings.theme.ui_font_size.map(|s| s.0) == Some(applied) {
            settings.theme.ui_font_size = snapshot.prior_ui_font_size.map(FontSize);
        }
    }
    if let Some(applied) = snapshot.applied_buffer_font_size {
        if settings.theme.buffer_font_size.map(|s| s.0) == Some(applied) {
            settings.theme.buffer_font_size = snapshot.prior_buffer_font_size.map(FontSize);
        }
    }
    if let Some(applied) = snapshot.applied_terminal_font_size {
        if current_terminal_font_size(settings) == Some(applied) {
            set_terminal_font_size(settings, snapshot.prior_terminal_font_size);
        }
    }

    if matches!(settings.zerminal.as_ref(), Some(z) if *z == ZerminalSettingsContent::default()) {
        settings.zerminal = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, VisualTestContext};
    use project::Project;
    use serde_json::json;
    use settings::SettingsContent;
    use theme::{Appearance, ThemeFamily, ThemeRegistry, default_color_scales};
    use util::path;
    use workspace::MultiWorkspace;

    fn fonts(ui: Option<&str>, buffer: Option<&str>, terminal: Option<&str>) -> ZerminalThemeFonts {
        ZerminalThemeFonts {
            ui_font_family: ui.map(|s| SharedString::new(s.to_string())),
            buffer_font_family: buffer.map(|s| SharedString::new(s.to_string())),
            terminal_font_family: terminal.map(|s| SharedString::new(s.to_string())),
            ui_font_size: None,
            buffer_font_size: None,
            terminal_font_size: None,
        }
    }

    fn fonts_with_sizes(
        ui: Option<(&str, f32)>,
        buffer: Option<(&str, f32)>,
        terminal: Option<(&str, f32)>,
    ) -> ZerminalThemeFonts {
        ZerminalThemeFonts {
            ui_font_family: ui.map(|(s, _)| SharedString::new(s.to_string())),
            buffer_font_family: buffer.map(|(s, _)| SharedString::new(s.to_string())),
            terminal_font_family: terminal.map(|(s, _)| SharedString::new(s.to_string())),
            ui_font_size: ui.map(|(_, n)| n),
            buffer_font_size: buffer.map(|(_, n)| n),
            terminal_font_size: terminal.map(|(_, n)| n),
        }
    }

    fn ui_family(s: &SettingsContent) -> Option<String> {
        s.theme.ui_font_family.as_ref().map(|f| f.0.to_string())
    }

    fn buffer_family(s: &SettingsContent) -> Option<String> {
        s.theme.buffer_font_family.as_ref().map(|f| f.0.to_string())
    }

    fn terminal_family(s: &SettingsContent) -> Option<String> {
        s.terminal
            .as_ref()
            .and_then(|t| t.font_family.as_ref())
            .map(|f| f.0.to_string())
    }

    #[test]
    fn paired_apply_snapshots_prior_fonts_and_writes_paired() {
        let mut settings = SettingsContent::default();
        settings.theme.ui_font_family = Some(FontFamilyName("Inter".into()));
        settings.theme.buffer_font_family = Some(FontFamilyName("Hack".into()));

        apply_paired_theme_fonts(
            &mut settings,
            &fonts(Some("Sora"), Some("Victor Mono"), Some("Victor Mono")),
        );

        assert_eq!(ui_family(&settings).as_deref(), Some("Sora"));
        assert_eq!(buffer_family(&settings).as_deref(), Some("Victor Mono"));
        assert_eq!(terminal_family(&settings).as_deref(), Some("Victor Mono"));

        let snapshot = settings
            .zerminal
            .as_ref()
            .and_then(|z| z.paired_theme_font_snapshot.as_ref())
            .expect("snapshot should be recorded on apply");
        assert_eq!(snapshot.prior_ui_font_family.as_deref(), Some("Inter"));
        assert_eq!(snapshot.prior_buffer_font_family.as_deref(), Some("Hack"));
        assert_eq!(snapshot.prior_terminal_font_family, None);
        assert_eq!(snapshot.applied_ui_font_family.as_deref(), Some("Sora"));
        assert_eq!(snapshot.applied_buffer_font_family.as_deref(), Some("Victor Mono"));
        assert_eq!(snapshot.applied_terminal_font_family.as_deref(), Some("Victor Mono"));
    }

    #[test]
    fn restore_reverts_paired_fonts_and_clears_snapshot() {
        let mut settings = SettingsContent::default();
        settings.theme.ui_font_family = Some(FontFamilyName("Inter".into()));
        apply_paired_theme_fonts(
            &mut settings,
            &fonts(Some("Sora"), Some("Victor Mono"), Some("Victor Mono")),
        );

        restore_paired_theme_fonts_if_any(&mut settings);

        assert_eq!(ui_family(&settings).as_deref(), Some("Inter"));
        assert_eq!(buffer_family(&settings), None);
        assert_eq!(terminal_family(&settings), None);
        assert!(settings.zerminal.is_none(), "snapshot should be cleared after restore");
    }

    #[test]
    fn restore_preserves_user_modified_paired_fonts() {
        let mut settings = SettingsContent::default();
        settings.theme.ui_font_family = Some(FontFamilyName("Inter".into()));
        apply_paired_theme_fonts(
            &mut settings,
            &fonts(Some("Sora"), Some("Victor Mono"), None),
        );

        // User reaches into settings and overrides the paired buffer font.
        settings.theme.buffer_font_family = Some(FontFamilyName("Comic Sans".into()));

        restore_paired_theme_fonts_if_any(&mut settings);

        assert_eq!(
            ui_family(&settings).as_deref(),
            Some("Inter"),
            "untouched paired slot should restore to prior"
        );
        assert_eq!(
            buffer_family(&settings).as_deref(),
            Some("Comic Sans"),
            "manually overridden paired slot should be preserved"
        );
    }

    #[test]
    fn paired_to_paired_keeps_original_snapshot_and_updates_applied() {
        let mut settings = SettingsContent::default();
        settings.theme.ui_font_family = Some(FontFamilyName("Inter".into()));
        settings.theme.buffer_font_family = Some(FontFamilyName("Hack".into()));

        apply_paired_theme_fonts(
            &mut settings,
            &fonts(Some("Sora"), Some("Mono A"), None),
        );
        // Now select a different paired theme that pairs ui + terminal but not buffer.
        apply_paired_theme_fonts(
            &mut settings,
            &fonts(Some("Sora 2"), None, Some("Term B")),
        );

        assert_eq!(ui_family(&settings).as_deref(), Some("Sora 2"));
        assert_eq!(
            buffer_family(&settings).as_deref(),
            Some("Mono A"),
            "buffer font should remain whatever was there since the new pairing doesn't touch it"
        );
        assert_eq!(terminal_family(&settings).as_deref(), Some("Term B"));

        let snapshot = settings
            .zerminal
            .as_ref()
            .and_then(|z| z.paired_theme_font_snapshot.as_ref())
            .expect("snapshot should still exist across paired switch");
        assert_eq!(
            snapshot.prior_ui_font_family.as_deref(),
            Some("Inter"),
            "prior values should reflect first paired switch, not subsequent ones"
        );
        assert_eq!(snapshot.prior_buffer_font_family.as_deref(), Some("Hack"));
        assert_eq!(snapshot.applied_ui_font_family.as_deref(), Some("Sora 2"));
        assert_eq!(
            snapshot.applied_buffer_font_family.as_deref(),
            Some("Mono A"),
            "applied buffer should still reflect the first paired theme since the second didn't touch it"
        );
        assert_eq!(snapshot.applied_terminal_font_family.as_deref(), Some("Term B"));
    }

    #[test]
    fn paired_apply_and_restore_handles_font_sizes() {
        let mut settings = SettingsContent::default();
        apply_paired_theme_fonts(
            &mut settings,
            &fonts_with_sizes(Some(("Sora", 16.0)), Some(("Victor Mono", 15.0)), None),
        );

        assert_eq!(settings.theme.ui_font_size.map(|s| s.0), Some(16.0));
        assert_eq!(settings.theme.buffer_font_size.map(|s| s.0), Some(15.0));

        let snapshot = settings
            .zerminal
            .as_ref()
            .and_then(|z| z.paired_theme_font_snapshot.as_ref())
            .expect("snapshot should exist after apply");
        assert_eq!(snapshot.prior_ui_font_size, None);
        assert_eq!(snapshot.applied_ui_font_size, Some(16.0));
        assert_eq!(snapshot.applied_buffer_font_size, Some(15.0));

        restore_paired_theme_fonts_if_any(&mut settings);

        assert_eq!(settings.theme.ui_font_size, None);
        assert_eq!(settings.theme.buffer_font_size, None);
    }

    #[test]
    fn restore_preserves_user_modified_paired_font_size() {
        let mut settings = SettingsContent::default();
        apply_paired_theme_fonts(
            &mut settings,
            &fonts_with_sizes(Some(("Sora", 16.0)), Some(("Victor Mono", 15.0)), None),
        );
        // User cranks the buffer size up while paired theme is active.
        settings.theme.buffer_font_size = Some(FontSize(18.0));

        restore_paired_theme_fonts_if_any(&mut settings);

        assert_eq!(
            settings.theme.buffer_font_size.map(|s| s.0),
            Some(18.0),
            "manually changed buffer size should not be reverted"
        );
        assert_eq!(
            settings.theme.ui_font_size, None,
            "untouched paired UI size should restore to None"
        );
    }

    #[test]
    fn restore_with_no_snapshot_is_a_noop() {
        let mut settings = SettingsContent::default();
        settings.theme.ui_font_family = Some(FontFamilyName("Inter".into()));
        restore_paired_theme_fonts_if_any(&mut settings);
        assert_eq!(ui_family(&settings).as_deref(), Some("Inter"));
        assert!(settings.zerminal.is_none());
    }

    fn init_test(cx: &mut TestAppContext) -> Arc<workspace::AppState> {
        cx.update(|cx| {
            let app_state = workspace::AppState::test(cx);
            settings::init(cx);
            theme::init(theme::LoadThemes::JustBase, cx);
            editor::init(cx);
            super::init(cx);
            app_state
        })
    }

    fn register_test_themes(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let registry = ThemeRegistry::global(cx);
            let base_theme = registry.get("One Dark").unwrap();

            let mut test_light = (*base_theme).clone();
            test_light.id = "test-light".to_string();
            test_light.name = "Test Light".into();
            test_light.appearance = Appearance::Light;

            let mut test_dark_a = (*base_theme).clone();
            test_dark_a.id = "test-dark-a".to_string();
            test_dark_a.name = "Test Dark A".into();

            let mut test_dark_b = (*base_theme).clone();
            test_dark_b.id = "test-dark-b".to_string();
            test_dark_b.name = "Test Dark B".into();

            registry.register_test_themes([ThemeFamily {
                id: "test-family".to_string(),
                name: "Test Family".into(),
                author: "test".into(),
                themes: vec![test_light, test_dark_a, test_dark_b],
                scales: default_color_scales(),
            }]);
        });
    }

    async fn setup_test(cx: &mut TestAppContext) -> Arc<workspace::AppState> {
        let app_state = init_test(cx);
        register_test_themes(cx);
        app_state
            .fs
            .as_fake()
            .insert_tree(path!("/test"), json!({}))
            .await;
        app_state
    }

    fn open_theme_selector(
        workspace: &Entity<workspace::Workspace>,
        cx: &mut VisualTestContext,
    ) -> Entity<Picker<ThemeSelectorDelegate>> {
        cx.dispatch_action(zed_actions::theme_selector::Toggle {
            themes_filter: None,
        });
        cx.run_until_parked();
        workspace.update(cx, |workspace, cx| {
            workspace
                .active_modal::<ThemeSelector>(cx)
                .expect("theme selector should be open")
                .read(cx)
                .picker
                .clone()
        })
    }

    fn selected_theme_name(
        picker: &Entity<Picker<ThemeSelectorDelegate>>,
        cx: &mut VisualTestContext,
    ) -> String {
        picker.read_with(cx, |picker, _| {
            picker
                .delegate
                .matches
                .get(picker.delegate.selected_index)
                .expect("selected index should point to a match")
                .string
                .clone()
        })
    }

    fn previewed_theme_name(
        picker: &Entity<Picker<ThemeSelectorDelegate>>,
        cx: &mut VisualTestContext,
    ) -> String {
        picker.read_with(cx, |picker, _| picker.delegate.new_theme.name.to_string())
    }

    #[gpui::test]
    async fn test_theme_selector_preserves_selection_on_empty_filter(cx: &mut TestAppContext) {
        let app_state = setup_test(cx).await;
        let project = Project::test(app_state.fs.clone(), [path!("/test").as_ref()], cx).await;
        let (multi_workspace, cx) =
            cx.add_window_view(|window, cx| MultiWorkspace::test_new(project.clone(), window, cx));
        let workspace =
            multi_workspace.read_with(cx, |multi_workspace, _| multi_workspace.workspace().clone());
        let picker = open_theme_selector(&workspace, cx);

        let target_index = picker.read_with(cx, |picker, _| {
            picker
                .delegate
                .matches
                .iter()
                .position(|m| m.string == "Test Light")
                .unwrap()
        });
        picker.update_in(cx, |picker, window, cx| {
            picker.set_selected_index(target_index, None, true, window, cx);
        });
        cx.run_until_parked();

        assert_eq!(previewed_theme_name(&picker, cx), "Test Light");

        picker.update_in(cx, |picker, window, cx| {
            picker.update_matches("zzz".to_string(), window, cx);
        });
        cx.run_until_parked();

        picker.update_in(cx, |picker, window, cx| {
            picker.update_matches("".to_string(), window, cx);
        });
        cx.run_until_parked();

        assert_eq!(
            selected_theme_name(&picker, cx),
            "Test Light",
            "selected theme should be preserved after clearing an empty filter"
        );
        assert_eq!(
            previewed_theme_name(&picker, cx),
            "Test Light",
            "previewed theme should be preserved after clearing an empty filter"
        );
    }
}
