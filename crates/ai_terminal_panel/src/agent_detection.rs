use std::path::PathBuf;

use icons::IconName;

#[derive(Clone, Debug)]
pub struct AiAgent {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub icon: IconName,
    pub path: PathBuf,
}

struct KnownAgent {
    name: &'static str,
    command: &'static str,
    icon: IconName,
    search_paths: &'static [&'static str],
}

const KNOWN_AGENTS: &[KnownAgent] = &[
    KnownAgent {
        name: "Claude Code",
        command: "claude",
        icon: IconName::AiClaude,
        search_paths: &[
            ".local/bin/claude",
            ".bun/bin/claude",
            ".npm-global/bin/claude",
            ".volta/bin/claude",
        ],
    },
    KnownAgent {
        name: "Codex",
        command: "codex",
        icon: IconName::AiOpenAi,
        search_paths: &[".local/bin/codex"],
    },
    KnownAgent {
        name: "OpenCode",
        command: "opencode",
        icon: IconName::AiOpenCode,
        search_paths: &[".local/bin/opencode"],
    },
    KnownAgent {
        name: "Aider",
        command: "aider",
        icon: IconName::Code,
        search_paths: &[
            ".local/bin/aider",
            ".local/pipx/venvs/aider-chat/bin/aider",
        ],
    },
    KnownAgent {
        name: "Goose",
        command: "goose",
        icon: IconName::BoltFilled,
        search_paths: &[".local/bin/goose"],
    },
];

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

pub fn detect_agents(custom_agents: &[super::CustomAgentConfig]) -> Vec<AiAgent> {
    let mut agents = Vec::new();

    for known in KNOWN_AGENTS {
        if let Some(path) = find_in_path(known.command, known.search_paths) {
            agents.push(AiAgent {
                name: known.name.to_string(),
                command: known.command.to_string(),
                args: Vec::new(),
                icon: known.icon,
                path,
            });
        }
    }

    // Append custom agents from settings
    for custom in custom_agents {
        let icon = custom
            .icon
            .as_deref()
            .and_then(|s| s.parse::<IconName>().ok())
            .unwrap_or(IconName::Sparkle);

        let path = find_in_path(&custom.command, &[])
            .unwrap_or_else(|| PathBuf::from(&custom.command));

        agents.push(AiAgent {
            name: custom.name.clone(),
            command: custom.command.clone(),
            args: custom.args.clone().unwrap_or_default(),
            icon,
            path,
        });
    }

    agents
}
