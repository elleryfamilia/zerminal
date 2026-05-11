use alacritty_terminal::vte::ansi::{
    CursorShape as AlacCursorShape, CursorStyle as AlacCursorStyle,
};
use collections::HashMap;
use gpui::{FontFallbacks, FontFeatures, FontWeight, Pixels, px};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub use settings::AlternateScroll;

use settings::{
    IntoGpui, PathHyperlinkRegex, RegisterSetting, ShowScrollbar, TerminalBlink,
    TerminalDockPosition, TerminalLineHeight, VenvSettings, WorkingDirectory,
    merge_from::MergeFrom,
};
use task::Shell;
use theme_settings::FontFamilyName;

#[derive(Copy, Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct Toolbar {
    pub breadcrumbs: bool,
}

#[derive(Clone, Debug, Deserialize, RegisterSetting)]
pub struct TerminalSettings {
    pub shell: Shell,
    pub working_directory: WorkingDirectory,
    pub font_size: Option<Pixels>, // todo(settings_refactor) can be non-optional...
    pub font_family: Option<FontFamilyName>,
    pub font_fallbacks: Option<FontFallbacks>,
    pub font_features: Option<FontFeatures>,
    pub font_weight: Option<FontWeight>,
    pub line_height: TerminalLineHeight,
    pub env: HashMap<String, String>,
    pub cursor_shape: CursorShape,
    pub blinking: TerminalBlink,
    pub alternate_scroll: AlternateScroll,
    pub option_as_meta: bool,
    pub copy_on_select: bool,
    pub keep_selection_on_copy: bool,
    pub button: bool,
    pub dock: TerminalDockPosition,
    pub flexible: bool,
    pub default_width: Pixels,
    pub default_height: Pixels,
    pub detect_venv: VenvSettings,
    pub max_scroll_history_lines: Option<usize>,
    pub scroll_multiplier: f32,
    pub toolbar: Toolbar,
    pub scrollbar: ScrollbarSettings,
    pub minimum_contrast: f32,
    pub path_hyperlink_regexes: Vec<String>,
    pub path_hyperlink_timeout_ms: u64,
    pub show_count_badge: bool,
    pub bell: TerminalBell,
    pub screensaver: ScreensaverSettings,
}

pub use settings::TerminalBell;

#[derive(Copy, Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct ScreensaverSettings {
    pub enabled: bool,
    pub idle_seconds: u64,
    pub theme: ScreensaverTheme,
    pub particle_count: usize,
    pub opacity: f32,
    pub fps: u32,
    pub gravity: f32,
    pub friction: f32,
}

impl Default for ScreensaverSettings {
    fn default() -> Self {
        ScreensaverSettings {
            enabled: false,
            idle_seconds: 120,
            theme: ScreensaverTheme::Cosmic,
            particle_count: 60,
            opacity: 0.7,
            fps: 30,
            gravity: 0.0,
            friction: 0.98,
        }
    }
}

#[derive(Copy, Clone, Debug, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ScreensaverTheme {
    #[default]
    Cosmic,
    Nord,
    Dracula,
    Catppuccin,
    Gruvbox,
    Forest,
    Wildberries,
    Mono,
    Rosepine,
}

impl ScreensaverTheme {
    pub fn name(self) -> &'static str {
        match self {
            ScreensaverTheme::Cosmic => "cosmic",
            ScreensaverTheme::Nord => "nord",
            ScreensaverTheme::Dracula => "dracula",
            ScreensaverTheme::Catppuccin => "catppuccin",
            ScreensaverTheme::Gruvbox => "gruvbox",
            ScreensaverTheme::Forest => "forest",
            ScreensaverTheme::Wildberries => "wildberries",
            ScreensaverTheme::Mono => "mono",
            ScreensaverTheme::Rosepine => "rosepine",
        }
    }

    pub fn from_user(name: &str) -> Self {
        match name.to_ascii_lowercase().as_str() {
            "nord" => ScreensaverTheme::Nord,
            "dracula" => ScreensaverTheme::Dracula,
            "catppuccin" => ScreensaverTheme::Catppuccin,
            "gruvbox" => ScreensaverTheme::Gruvbox,
            "forest" => ScreensaverTheme::Forest,
            "wildberries" => ScreensaverTheme::Wildberries,
            "mono" => ScreensaverTheme::Mono,
            "rosepine" => ScreensaverTheme::Rosepine,
            _ => ScreensaverTheme::Cosmic,
        }
    }
}

#[derive(Copy, Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ScrollbarSettings {
    /// When to show the scrollbar in the terminal.
    ///
    /// Default: inherits editor scrollbar settings
    pub show: Option<ShowScrollbar>,
}

fn settings_shell_to_task_shell(shell: settings::Shell) -> Shell {
    match shell {
        settings::Shell::System => Shell::System,
        settings::Shell::Program(program) => Shell::Program(program),
        settings::Shell::WithArguments {
            program,
            args,
            title_override,
        } => Shell::WithArguments {
            program,
            args,
            title_override,
        },
    }
}

impl settings::Settings for TerminalSettings {
    fn from_settings(content: &settings::SettingsContent) -> Self {
        let user_content = content.terminal.clone().unwrap();
        // Note: we allow a subset of "terminal" settings in the project files.
        let mut project_content = user_content.project.clone();
        project_content.merge_from_option(content.project.terminal.as_ref());
        TerminalSettings {
            shell: settings_shell_to_task_shell(project_content.shell.unwrap()),
            working_directory: project_content.working_directory.unwrap(),
            font_size: user_content.font_size.map(|s| s.into_gpui()),
            font_family: user_content.font_family,
            font_fallbacks: user_content.font_fallbacks.map(|fallbacks| {
                FontFallbacks::from_fonts(
                    fallbacks
                        .into_iter()
                        .map(|family| family.0.to_string())
                        .collect(),
                )
            }),
            font_features: user_content.font_features.map(|f| f.into_gpui()),
            font_weight: user_content.font_weight.map(|w| w.into_gpui()),
            line_height: user_content.line_height.unwrap(),
            env: project_content.env.unwrap(),
            cursor_shape: user_content.cursor_shape.unwrap().into(),
            blinking: user_content.blinking.unwrap(),
            alternate_scroll: user_content.alternate_scroll.unwrap(),
            option_as_meta: user_content.option_as_meta.unwrap(),
            copy_on_select: user_content.copy_on_select.unwrap(),
            keep_selection_on_copy: user_content.keep_selection_on_copy.unwrap(),
            button: user_content.button.unwrap(),
            dock: user_content.dock.unwrap(),
            default_width: px(user_content.default_width.unwrap()),
            default_height: px(user_content.default_height.unwrap()),
            flexible: user_content.flexible.unwrap(),
            detect_venv: project_content.detect_venv.unwrap(),
            scroll_multiplier: user_content.scroll_multiplier.unwrap(),
            max_scroll_history_lines: user_content.max_scroll_history_lines,
            toolbar: Toolbar {
                breadcrumbs: user_content.toolbar.unwrap().breadcrumbs.unwrap(),
            },
            scrollbar: ScrollbarSettings {
                show: user_content.scrollbar.unwrap().show,
            },
            minimum_contrast: user_content.minimum_contrast.unwrap(),
            path_hyperlink_regexes: project_content
                .path_hyperlink_regexes
                .unwrap()
                .into_iter()
                .map(|regex| match regex {
                    PathHyperlinkRegex::SingleLine(regex) => regex,
                    PathHyperlinkRegex::MultiLine(regex) => regex.join("\n"),
                })
                .collect(),
            path_hyperlink_timeout_ms: project_content.path_hyperlink_timeout_ms.unwrap(),
            show_count_badge: user_content.show_count_badge.unwrap(),
            bell: user_content.bell.unwrap_or_default(),
            screensaver: screensaver_settings_from_content(user_content.screensaver.as_ref()),
        }
    }
}

fn screensaver_settings_from_content(
    content: Option<&settings::TerminalScreensaverContent>,
) -> ScreensaverSettings {
    let defaults = ScreensaverSettings::default();
    let Some(content) = content else {
        return defaults;
    };
    ScreensaverSettings {
        enabled: content.enabled.unwrap_or(defaults.enabled),
        idle_seconds: content
            .idle_seconds
            .map(|seconds| seconds.clamp(10, 86_400))
            .unwrap_or(defaults.idle_seconds),
        theme: content
            .theme
            .as_deref()
            .map(ScreensaverTheme::from_user)
            .unwrap_or(defaults.theme),
        particle_count: content
            .particle_count
            .map(|count| count.clamp(1, 500))
            .unwrap_or(defaults.particle_count),
        opacity: content
            .opacity
            .map(|opacity| opacity.clamp(0.0, 1.0))
            .unwrap_or(defaults.opacity),
        fps: content
            .fps
            .map(|fps| fps.clamp(5, 60))
            .unwrap_or(defaults.fps),
        gravity: content
            .gravity
            .map(|gravity| gravity.clamp(-1.0, 1.0))
            .unwrap_or(defaults.gravity),
        friction: content
            .friction
            .map(|friction| friction.clamp(0.0, 1.0))
            .unwrap_or(defaults.friction),
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CursorShape {
    /// Cursor is a block like `█`.
    #[default]
    Block,
    /// Cursor is an underscore like `_`.
    Underline,
    /// Cursor is a vertical bar like `⎸`.
    Bar,
    /// Cursor is a hollow box like `▯`.
    Hollow,
}

impl From<settings::CursorShapeContent> for CursorShape {
    fn from(value: settings::CursorShapeContent) -> Self {
        match value {
            settings::CursorShapeContent::Block => CursorShape::Block,
            settings::CursorShapeContent::Underline => CursorShape::Underline,
            settings::CursorShapeContent::Bar => CursorShape::Bar,
            settings::CursorShapeContent::Hollow => CursorShape::Hollow,
        }
    }
}

impl From<CursorShape> for AlacCursorShape {
    fn from(value: CursorShape) -> Self {
        match value {
            CursorShape::Block => AlacCursorShape::Block,
            CursorShape::Underline => AlacCursorShape::Underline,
            CursorShape::Bar => AlacCursorShape::Beam,
            CursorShape::Hollow => AlacCursorShape::HollowBlock,
        }
    }
}

impl From<CursorShape> for AlacCursorStyle {
    fn from(value: CursorShape) -> Self {
        AlacCursorStyle {
            shape: value.into(),
            blinking: false,
        }
    }
}
