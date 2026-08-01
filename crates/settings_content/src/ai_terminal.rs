use collections::HashMap;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use settings_macros::{MergeFrom, with_fallible_options};

/// Customizes how AI agent CLIs are launched in the AI terminal panel.
#[with_fallible_options]
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema, MergeFrom)]
pub struct AiTerminalSettingsContent {
    /// Per-agent launch overrides, keyed by agent id.
    ///
    /// A key that matches a detected tool ("claude", "codex", "copilot",
    /// "opencode", "aider", "goose") customizes how that tool is launched —
    /// you can pass extra args, set environment variables, or point at a
    /// different binary while keeping its built-in integrations (e.g. Claude
    /// and Copilot `/ide`).
    ///
    /// Any other key defines a brand-new agent that shows up alongside the
    /// detected ones; such an entry must set `command`.
    pub agents: Option<HashMap<String, AiTerminalAgentSettings>>,
    /// Wraps agent launches in an external launcher command (e.g. a context
    /// renderer like `load run claude`, `distrobox enter dev --`, or an env
    /// shim). Applies to every agent unless `launcher.agents` narrows it.
    pub launcher: Option<AiTerminalLauncherSettings>,
}

/// Launch customization for a single AI agent CLI.
#[with_fallible_options]
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema, MergeFrom)]
pub struct AiTerminalAgentSettings {
    /// Program to run: a binary name resolved on `PATH`, or an absolute path.
    ///
    /// Optional for detected tools (defaults to the auto-detected binary);
    /// required when defining a new agent.
    pub command: Option<String>,
    /// Arguments passed to the command. Replaces the default arguments.
    pub args: Option<Vec<String>>,
    /// Extra environment variables for the agent process, merged over the
    /// inherited environment by key. For agents with `/ide` integration
    /// (Claude, Copilot), `HOME` and `XDG_STATE_HOME` are ignored here so the
    /// CLI keeps finding Zerminal's lockfile.
    pub env: Option<HashMap<String, String>>,
    /// Display name shown in the panel. Defaults to the detected tool's name,
    /// or the key for a new agent.
    pub name: Option<String>,
    /// Icon name (see `IconName`). Defaults to the detected tool's icon, or a
    /// generic sparkle for a new agent.
    pub icon: Option<String>,
}

/// Wraps AI agent launches in an external launcher command.
#[with_fallible_options]
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema, MergeFrom)]
pub struct AiTerminalLauncherSettings {
    /// Whether agent launches are wrapped when `command` is set.
    ///
    /// Default: true
    pub enabled: Option<bool>,
    /// Wrapper command template, shell-split at launch time (quotes
    /// respected). `{agent}` is replaced with the agent id ("claude",
    /// "codex", …) and `{command}` with the resolved agent executable path.
    /// A template with neither placeholder is used as a prefix: the agent
    /// executable is appended after it. The agent's own arguments are always
    /// appended last. Empty or unset disables wrapping.
    ///
    /// Default: ""
    pub command: Option<String>,
    /// Agent ids the wrapper applies to. Unset applies it to all agents.
    pub agents: Option<Vec<String>>,
}
