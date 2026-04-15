use std::path::PathBuf;

use coding_tools::KNOWN_TOOLS;
use icons::IconName;

#[derive(Clone, Debug)]
pub struct AiAgent {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub icon: IconName,
    pub path: PathBuf,
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

pub fn detect_agents(custom_agents: &[super::CustomAgentConfig]) -> Vec<AiAgent> {
    let mut agents = Vec::new();

    for tool in KNOWN_TOOLS {
        if let Some(path) = find_in_path(tool.command, tool.binary_search_paths) {
            agents.push(AiAgent {
                name: tool.name.to_string(),
                command: tool.command.to_string(),
                args: Vec::new(),
                icon: tool.icon,
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
