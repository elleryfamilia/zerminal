use std::collections::BTreeMap;
use std::path::PathBuf;

use coding_tools::KNOWN_TOOLS;
use collections::HashMap;
use icons::IconName;
use settings::{RegisterSetting, Settings, SettingsContent};

/// A launchable AI agent CLI, either auto-detected from [`KNOWN_TOOLS`] or
/// defined/customized by the user via [`AiTerminalSettings`].
#[derive(Clone, Debug)]
pub struct AiAgent {
    /// Stable identity. For detected tools this is the tool's command
    /// ("claude", "codex", …); for user-defined agents it's the settings key.
    /// Drives `/ide` gating and the spawned task id — NOT the executable, which
    /// is [`AiAgent::path`].
    pub id: String,
    pub name: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    pub icon: IconName,
    /// Resolved executable to run.
    pub path: PathBuf,
}

/// Resolved per-agent launch customization (see [`AiTerminalSettings`]).
#[derive(Clone, Debug, Default)]
pub struct AiTerminalAgentConfig {
    pub command: Option<String>,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    pub name: Option<String>,
    pub icon: Option<String>,
}

/// Resolved launch-wrapper configuration (see `AiTerminalLauncherSettings`).
#[derive(Clone, Debug, PartialEq)]
pub struct LauncherConfig {
    pub enabled: bool,
    pub command: String,
    pub agents: Option<Vec<String>>,
}

/// Resolved AI terminal settings: per-agent launch overrides keyed by agent id.
/// Backed by a `BTreeMap` so the launcher/menu order is deterministic.
#[derive(Clone, Debug, Default, RegisterSetting)]
pub struct AiTerminalSettings {
    pub agents: BTreeMap<String, AiTerminalAgentConfig>,
    pub launcher: Option<LauncherConfig>,
}

impl Settings for AiTerminalSettings {
    fn from_settings(content: &SettingsContent) -> Self {
        let content = content.ai_terminal.clone().unwrap_or_default();
        let agents = content
            .agents
            .unwrap_or_default()
            .into_iter()
            .map(|(id, agent)| {
                (
                    id,
                    AiTerminalAgentConfig {
                        command: agent.command,
                        args: agent.args.unwrap_or_default(),
                        env: agent.env.unwrap_or_default(),
                        name: agent.name,
                        icon: agent.icon,
                    },
                )
            })
            .collect();
        let launcher = content.launcher.map(|launcher| LauncherConfig {
            enabled: launcher.enabled.unwrap_or(true),
            command: launcher.command.unwrap_or_default(),
            agents: launcher.agents,
        });
        Self { agents, launcher }
    }
}

const SYSTEM_PATHS: &[&str] = &["/usr/local/bin", "/usr/bin", "/opt/homebrew/bin"];

fn find_in_path(command: &str, search_paths: &[&str]) -> Option<PathBuf> {
    let home = dirs::home_dir()?;

    // Check agent-specific paths relative to home
    for relative in search_paths {
        let path = home.join(relative);
        if path.exists() {
            return Some(path);
        }
    }

    // Check system paths
    for dir in SYSTEM_PATHS {
        let path = PathBuf::from(dir).join(command);
        if path.exists() {
            return Some(path);
        }
    }

    None
}

/// Resolves an override's `command` to an executable path: an absolute/relative
/// path is used as-is, a bare name is looked up on `PATH`, falling back to the
/// literal so the spawn surfaces a clear "not found" error rather than silently
/// dropping the agent.
fn resolve_command(config: &AiTerminalAgentConfig) -> Option<PathBuf> {
    let command = config.command.as_deref()?;
    Some(find_in_path(command, &[]).unwrap_or_else(|| PathBuf::from(command)))
}

fn parse_icon(icon: Option<&str>) -> Option<IconName> {
    icon.and_then(|name| name.parse::<IconName>().ok())
}

/// Applies a user override to an existing agent. `command` repoints the binary,
/// `args` replace the defaults, `env` merges by key (override wins), and
/// `name`/`icon` replace when set. The agent's `id` (identity) is never changed.
fn apply_override(agent: &mut AiAgent, config: &AiTerminalAgentConfig) {
    if let Some(path) = resolve_command(config) {
        agent.path = path;
    }
    agent.args = config.args.clone();
    for (key, value) in &config.env {
        agent.env.insert(key.clone(), value.clone());
    }
    if let Some(name) = &config.name {
        agent.name = name.clone();
    }
    if let Some(icon) = parse_icon(config.icon.as_deref()) {
        agent.icon = icon;
    }
}

/// Detects available AI agent CLIs, then layers user customization on top:
///
/// 1. Each tool in [`KNOWN_TOOLS`] found on disk becomes an agent (in
///    `KNOWN_TOOLS` order).
/// 2. An override keyed by a detected agent's id customizes it in place.
/// 3. An override keyed by a known tool that wasn't found is materialized when
///    it supplies a `command` (lets users point at a non-standard install).
/// 4. Any other override key defines a brand-new agent; it must supply a
///    `command`, otherwise it is skipped with a warning.
pub fn detect_agents(overrides: &BTreeMap<String, AiTerminalAgentConfig>) -> Vec<AiAgent> {
    let mut agents: Vec<AiAgent> = Vec::new();

    for tool in KNOWN_TOOLS {
        if let Some(path) = find_in_path(tool.command, tool.binary_search_paths) {
            agents.push(AiAgent {
                id: tool.command.to_string(),
                name: tool.name.to_string(),
                args: Vec::new(),
                env: HashMap::default(),
                icon: tool.icon,
                path,
            });
        }
    }

    for (id, config) in overrides {
        if let Some(agent) = agents.iter_mut().find(|agent| &agent.id == id) {
            apply_override(agent, config);
        } else if let Some(tool) = KNOWN_TOOLS.iter().find(|tool| tool.command == id) {
            let Some(path) = resolve_command(config) else {
                continue;
            };
            let mut agent = AiAgent {
                id: id.clone(),
                name: tool.name.to_string(),
                args: Vec::new(),
                env: HashMap::default(),
                icon: tool.icon,
                path,
            };
            apply_override(&mut agent, config);
            agents.push(agent);
        } else {
            let Some(path) = resolve_command(config) else {
                log::warn!(
                    "ai_terminal agent {id:?} defines no command and matches no known tool; skipping"
                );
                continue;
            };
            agents.push(AiAgent {
                id: id.clone(),
                name: config.name.clone().unwrap_or_else(|| id.clone()),
                args: config.args.clone(),
                env: config.env.clone(),
                icon: parse_icon(config.icon.as_deref()).unwrap_or(IconName::Sparkle),
                path,
            });
        }
    }

    agents
}

/// The wrapped `(program, args)` for launching `agent` through the configured
/// launcher, or `None` when the launch should be raw: no launcher, disabled,
/// empty or malformed template, or the agent filtered out by the allowlist.
/// The wrapper is an enhancement, never a gate.
pub fn wrapped_launch(
    agent: &AiAgent,
    launcher: Option<&LauncherConfig>,
) -> Option<(PathBuf, Vec<String>)> {
    let launcher = launcher?;
    if !launcher.enabled || launcher.command.trim().is_empty() {
        return None;
    }
    if let Some(allowlist) = &launcher.agents
        && !allowlist.iter().any(|id| id == &agent.id)
    {
        return None;
    }
    let Some(template) = shlex::split(&launcher.command) else {
        return None;
    };
    if template.is_empty() {
        return None;
    }

    let has_placeholder = template
        .iter()
        .any(|part| part.contains("{agent}") || part.contains("{command}"));
    let agent_command = agent.path.to_string_lossy();
    let mut argv: Vec<String> = template
        .iter()
        .map(|part| {
            part.replace("{agent}", &agent.id)
                .replace("{command}", &agent_command)
        })
        .collect();
    if !has_placeholder {
        argv.push(agent_command.to_string());
    }
    argv.extend(agent.args.iter().cloned());

    let program = argv.remove(0);
    let path = find_in_path(&program, &[]).unwrap_or_else(|| PathBuf::from(&program));
    Some((path, argv))
}

/// The `(program, args)` actually spawned for `agent`: the wrapped launch
/// when the launcher applies, the agent's own command otherwise.
pub fn launch_command(
    agent: &AiAgent,
    launcher: Option<&LauncherConfig>,
) -> (PathBuf, Vec<String>) {
    wrapped_launch(agent, launcher).unwrap_or_else(|| {
        // Warn only on the spawn path: launcher_hint also calls
        // wrapped_launch on every render, which would spam the log.
        if let Some(launcher) = launcher
            && launcher.enabled
            && !launcher.command.trim().is_empty()
            && shlex::split(&launcher.command).is_none()
        {
            log::warn!(
                "ai_terminal launcher template {:?} failed to parse; launching {:?} raw",
                launcher.command,
                agent.id
            );
        }
        (agent.path.clone(), agent.args.clone())
    })
}

/// Short hint for wrapped launches ("via load"), derived from the wrapper
/// program's file name. `None` when the launch is raw.
pub fn launcher_hint(agent: &AiAgent, launcher: Option<&LauncherConfig>) -> Option<String> {
    let (path, _) = wrapped_launch(agent, launcher)?;
    let name = path.file_name()?.to_string_lossy();
    Some(format!("via {name}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(command: Option<&str>) -> AiTerminalAgentConfig {
        AiTerminalAgentConfig {
            command: command.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn new_agent_requires_command() {
        let mut overrides = BTreeMap::new();
        overrides.insert("with-command".to_string(), config(Some("/bin/echo")));
        overrides.insert("no-command".to_string(), config(None));

        let agents = detect_agents(&overrides);

        assert!(agents.iter().any(|agent| agent.id == "with-command"));
        assert!(
            !agents.iter().any(|agent| agent.id == "no-command"),
            "an agent without a command and matching no known tool must be skipped"
        );
    }

    #[test]
    fn new_agent_defaults_name_to_key() {
        let mut overrides = BTreeMap::new();
        overrides.insert("my-agent".to_string(), config(Some("/bin/echo")));

        let agent = detect_agents(&overrides)
            .into_iter()
            .find(|agent| agent.id == "my-agent")
            .expect("new agent should be present");

        assert_eq!(agent.name, "my-agent");
        assert_eq!(agent.path, PathBuf::from("/bin/echo"));
    }

    #[test]
    fn override_replaces_args_and_merges_env() {
        let mut agent = AiAgent {
            id: "claude".to_string(),
            name: "Claude Code".to_string(),
            args: vec!["old".to_string()],
            env: HashMap::from_iter([("KEEP".to_string(), "1".to_string())]),
            icon: IconName::AiClaude,
            path: PathBuf::from("/usr/bin/claude"),
        };
        let override_config = AiTerminalAgentConfig {
            command: None,
            args: vec!["--model".to_string(), "opus".to_string()],
            env: HashMap::from_iter([("ADDED".to_string(), "2".to_string())]),
            name: None,
            icon: None,
        };

        apply_override(&mut agent, &override_config);

        assert_eq!(agent.args, vec!["--model".to_string(), "opus".to_string()]);
        assert_eq!(agent.env.get("KEEP").map(String::as_str), Some("1"));
        assert_eq!(agent.env.get("ADDED").map(String::as_str), Some("2"));
        assert_eq!(agent.id, "claude");
        assert_eq!(agent.icon, IconName::AiClaude);
    }

    fn wrapper_test_agent() -> AiAgent {
        AiAgent {
            id: "claude".to_string(),
            name: "Claude Code".to_string(),
            args: vec!["--model".to_string(), "opus".to_string()],
            env: HashMap::default(),
            icon: IconName::AiClaude,
            path: PathBuf::from("/usr/bin/claude"),
        }
    }

    fn launcher(command: &str) -> LauncherConfig {
        LauncherConfig {
            enabled: true,
            command: command.to_string(),
            agents: None,
        }
    }

    #[test]
    fn no_launcher_launches_raw() {
        let agent = wrapper_test_agent();
        let (program, args) = launch_command(&agent, None);
        assert_eq!(program, PathBuf::from("/usr/bin/claude"));
        assert_eq!(args, agent.args);
    }

    #[test]
    fn agent_placeholder_is_substituted() {
        let agent = wrapper_test_agent();
        // Absolute wrapper path: find_in_path falls back to the literal for
        // paths that don't exist, keeping this test machine-independent.
        let launcher = launcher("/opt/wrap/load run {agent} --");
        let (program, args) = launch_command(&agent, Some(&launcher));
        assert_eq!(program, PathBuf::from("/opt/wrap/load"));
        assert_eq!(args, vec!["run", "claude", "--", "--model", "opus"]);
    }

    #[test]
    fn command_placeholder_is_substituted() {
        let agent = wrapper_test_agent();
        let launcher = launcher("/opt/wrap/sandbox {command}");
        let (_, args) = launch_command(&agent, Some(&launcher));
        assert_eq!(args, vec!["/usr/bin/claude", "--model", "opus"]);
    }

    #[test]
    fn prefix_template_appends_agent_command() {
        let agent = wrapper_test_agent();
        let launcher = launcher("/opt/wrap/env FOO=1");
        let (program, args) = launch_command(&agent, Some(&launcher));
        assert_eq!(program, PathBuf::from("/opt/wrap/env"));
        assert_eq!(args, vec!["FOO=1", "/usr/bin/claude", "--model", "opus"]);
    }

    #[test]
    fn allowlist_filters_agents() {
        let agent = wrapper_test_agent();
        let mut config = launcher("/opt/wrap/load run {agent}");
        config.agents = Some(vec!["codex".to_string()]);
        assert!(wrapped_launch(&agent, Some(&config)).is_none());
        config.agents = Some(vec!["claude".to_string()]);
        assert!(wrapped_launch(&agent, Some(&config)).is_some());
    }

    #[test]
    fn disabled_or_empty_launcher_is_raw() {
        let agent = wrapper_test_agent();
        let mut config = launcher("/opt/wrap/load run {agent}");
        config.enabled = false;
        assert!(wrapped_launch(&agent, Some(&config)).is_none());
        assert!(wrapped_launch(&agent, Some(&launcher(""))).is_none());
        assert!(wrapped_launch(&agent, Some(&launcher("   "))).is_none());
    }

    #[test]
    fn malformed_template_is_raw() {
        let agent = wrapper_test_agent();
        let config = launcher("/opt/wrap/load \"unclosed");
        assert!(wrapped_launch(&agent, Some(&config)).is_none());
    }

    #[test]
    fn comment_only_template_is_raw() {
        // shlex strips `#` comments, so this splits to an empty template.
        let agent = wrapper_test_agent();
        assert!(wrapped_launch(&agent, Some(&launcher("# disabled"))).is_none());
    }

    #[test]
    fn quoted_segments_survive_splitting() {
        let agent = wrapper_test_agent();
        let config = launcher("\"/opt/my wrap/load\" run {agent}");
        let (program, args) = launch_command(&agent, Some(&config));
        assert_eq!(program, PathBuf::from("/opt/my wrap/load"));
        assert_eq!(args, vec!["run", "claude", "--model", "opus"]);
    }

    #[test]
    fn launcher_hint_uses_program_name() {
        let agent = wrapper_test_agent();
        let config = launcher("/opt/wrap/load run {agent} --");
        assert_eq!(
            launcher_hint(&agent, Some(&config)),
            Some("via load".to_string())
        );
        assert_eq!(launcher_hint(&agent, None), None);
    }
}
