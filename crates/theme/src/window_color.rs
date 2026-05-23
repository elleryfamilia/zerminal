use std::sync::Arc;

use gpui::{Hsla, rgb};

use crate::Theme;

/// A named per-window color scheme for the Type Shii theme. Each scheme is a
/// hand-tuned combination of a dark background and a bright, playfully
/// contrasting chrome (title bar + status bar), in the spirit of Type Shii's
/// default cyan-on-purple look. Used to make each project's window distinct.
pub struct WindowColorScheme {
    /// Stable key persisted in settings. Do not rename once shipped.
    pub key: &'static str,
    /// Display name (Type Shii flavored Gen Z slang) shown in the picker.
    pub label: &'static str,
    /// Window/editor chrome base: window background, toolbar, tab bar.
    pub background: Hsla,
    /// Raised surface: editor, terminal, active tab, panels.
    pub surface: Hsla,
    /// The playful accent applied to the title bar and status bar.
    pub chrome: Hsla,
    /// Foreground (text/icons) drawn on top of `chrome`, kept dark for contrast.
    pub chrome_foreground: Hsla,
}

fn color(hex: u32) -> Hsla {
    rgb(hex).into()
}

fn lighten(color: Hsla, amount: f32) -> Hsla {
    Hsla {
        l: (color.l + amount).min(1.0),
        ..color
    }
}

/// The curated set of window color schemes, in display order. The first entry
/// ("Aura") reproduces Type Shii's native palette.
pub fn window_color_schemes() -> Vec<WindowColorScheme> {
    vec![
        WindowColorScheme {
            key: "aura",
            label: "Aura",
            background: color(0x1d1234),
            surface: color(0x352158),
            chrome: color(0x27d3f5),
            chrome_foreground: color(0x04121a),
        },
        WindowColorScheme {
            key: "slay",
            label: "Slay",
            background: color(0x1f0f1c),
            surface: color(0x361f31),
            chrome: color(0xff4fa3),
            chrome_foreground: color(0x230614),
        },
        WindowColorScheme {
            key: "glow_up",
            label: "Glow Up",
            background: color(0x1c1408),
            surface: color(0x322813),
            chrome: color(0xffd84a),
            chrome_foreground: color(0x1d1500),
        },
        WindowColorScheme {
            key: "drip",
            label: "Drip",
            background: color(0x0e1626),
            surface: color(0x1c2944),
            chrome: color(0x45c4ff),
            chrome_foreground: color(0x04101c),
        },
        WindowColorScheme {
            key: "bussin",
            label: "Bussin",
            background: color(0x0d1d14),
            surface: color(0x163525),
            chrome: color(0x46e39a),
            chrome_foreground: color(0x042014),
        },
        WindowColorScheme {
            key: "sheesh",
            label: "Sheesh",
            background: color(0x1f1208),
            surface: color(0x352010),
            chrome: color(0xff7a3c),
            chrome_foreground: color(0x230d02),
        },
        WindowColorScheme {
            key: "demure",
            label: "Demure",
            background: color(0x14131c),
            surface: color(0x242235),
            chrome: color(0xc2b3ff),
            chrome_foreground: color(0x130f28),
        },
    ]
}

/// Resolves a persisted scheme key to its scheme, if it names a known one.
pub fn resolve_scheme(key: &str) -> Option<WindowColorScheme> {
    window_color_schemes()
        .into_iter()
        .find(|scheme| scheme.key == key)
}

/// Whether per-window color schemes apply to this theme. Currently scoped to the
/// Type Shii theme family (the only variant ships as "Type Shii Dark").
pub fn is_supported(theme: &Theme) -> bool {
    theme.name.starts_with("Type Shii")
}

/// Returns a clone of `base` recolored to `scheme`: the background/surface slots
/// and the title/status bar chrome are replaced wholesale, with chrome
/// foregrounds set for contrast. Syntax, accents, and player colors are left
/// intact (they're already tuned for Type Shii's dark surfaces).
pub fn apply_scheme(base: &Arc<Theme>, scheme: &WindowColorScheme) -> Arc<Theme> {
    let mut theme = (**base).clone();
    let colors = &mut theme.styles.colors;

    // Window chrome base.
    colors.background = scheme.background;
    colors.toolbar_background = scheme.background;
    colors.tab_bar_background = scheme.background;
    colors.tab_inactive_background = scheme.background;

    // Raised surfaces.
    colors.editor_background = scheme.surface;
    colors.terminal_background = scheme.surface;
    colors.terminal_ansi_background = scheme.surface;
    colors.tab_active_background = scheme.surface;
    colors.panel_background = scheme.surface;
    colors.elevated_surface_background = scheme.surface;
    colors.surface_background = scheme.surface;

    // Playful accent chrome: title bar + status bar.
    colors.title_bar_background = scheme.chrome;
    colors.title_bar_inactive_background = scheme.chrome;
    colors.status_bar_background = scheme.chrome;

    // Borders derived from the background, with the focused pane picking up the
    // chrome accent so the active split reads clearly.
    colors.border = lighten(scheme.background, 0.12);
    colors.border_variant = lighten(scheme.background, 0.08);
    colors.pane_group_border = lighten(scheme.background, 0.10);
    colors.pane_focused_border = scheme.chrome;

    // Keep chrome text/icons legible on the bright accent.
    theme.zerminal_title_bar_foreground = Some(scheme.chrome_foreground);
    theme.zerminal_status_bar_foreground = Some(scheme.chrome_foreground);

    Arc::new(theme)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fallback_themes::zed_default_dark;

    #[test]
    fn resolve_scheme_matches_known_keys_only() {
        assert!(resolve_scheme("aura").is_some());
        assert!(resolve_scheme("slay").is_some());
        assert!(resolve_scheme("not-a-real-scheme").is_none());
    }

    #[test]
    fn is_supported_only_for_type_shii_family() {
        let mut theme = zed_default_dark();
        assert!(!is_supported(&theme));
        theme.name = "Type Shii Dark".into();
        assert!(is_supported(&theme));
    }

    #[test]
    fn apply_scheme_recolors_chrome_but_keeps_text() {
        let base = Arc::new(zed_default_dark());
        let scheme = resolve_scheme("slay").expect("slay scheme exists");
        let themed = apply_scheme(&base, &scheme);

        assert_eq!(themed.styles.colors.title_bar_background, scheme.chrome);
        assert_eq!(themed.styles.colors.status_bar_background, scheme.chrome);
        assert_eq!(themed.styles.colors.background, scheme.background);
        assert_eq!(
            themed.zerminal_title_bar_foreground,
            Some(scheme.chrome_foreground)
        );
        // Foreground text is untouched so legibility is preserved.
        assert_eq!(base.styles.colors.text, themed.styles.colors.text);
    }
}
