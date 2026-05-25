use collections::HashMap;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use settings_macros::{MergeFrom, with_fallible_options};

/// Zerminal-only settings managed by the application.
///
/// Treated as internal state; users may inspect but should not edit.
#[with_fallible_options]
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema, MergeFrom)]
pub struct ZerminalSettingsContent {
    /// Snapshot of font settings captured the first time a paired-font theme
    /// was applied, used to restore prior fonts when the user later switches
    /// to a non-paired theme. Cleared automatically on restore.
    pub paired_theme_font_snapshot: Option<PairedThemeFontSnapshot>,
    /// Per-project window color scheme, keyed by absolute project path. The
    /// value is a color scheme key (see `theme::window_color_schemes`). Applied
    /// only when the active theme supports it (currently the Type Shii family)
    /// so that windows for different projects are visually distinguishable.
    pub window_colors: Option<HashMap<String, String>>,
}

#[with_fallible_options]
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema, MergeFrom)]
pub struct PairedThemeFontSnapshot {
    /// UI font family the user had configured before the first paired theme
    /// was applied. `None` if the user had no explicit `ui_font_family`.
    pub prior_ui_font_family: Option<String>,
    pub prior_buffer_font_family: Option<String>,
    pub prior_terminal_font_family: Option<String>,
    /// UI font family that the most recently applied paired theme wrote to
    /// settings. Used to detect whether the user has manually overridden a
    /// paired font: if the current value differs, restore leaves it alone.
    pub applied_ui_font_family: Option<String>,
    pub applied_buffer_font_family: Option<String>,
    pub applied_terminal_font_family: Option<String>,
    /// Prior font sizes mirroring the prior_*_font_family fields above.
    pub prior_ui_font_size: Option<f32>,
    pub prior_buffer_font_size: Option<f32>,
    pub prior_terminal_font_size: Option<f32>,
    /// Applied font sizes mirroring the applied_*_font_family fields above.
    pub applied_ui_font_size: Option<f32>,
    pub applied_buffer_font_size: Option<f32>,
    pub applied_terminal_font_size: Option<f32>,
    /// Prior buffer/terminal font fallback lists. `None` distinguishes "user
    /// had no fallbacks set" from `Some(vec![])` (empty list explicitly set).
    pub prior_buffer_font_fallbacks: Option<Vec<String>>,
    pub prior_terminal_font_fallbacks: Option<Vec<String>>,
    /// Fallback lists the most recently applied paired theme wrote. Used the
    /// same way as `applied_*_font_family`: if the current settings still
    /// match this, restore will rewrite back to the prior list; otherwise the
    /// user's edit wins.
    pub applied_buffer_font_fallbacks: Option<Vec<String>>,
    pub applied_terminal_font_fallbacks: Option<Vec<String>>,
}
