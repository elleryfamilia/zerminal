use icons::IconName;

pub struct MemoryFileSpec {
    pub pattern: &'static str,
    pub is_dir: bool,
    pub subdirs: &'static [&'static str],
}

pub struct CodingTool {
    pub name: &'static str,
    pub command: &'static str,
    pub icon: IconName,
    pub binary_search_paths: &'static [&'static str],
    pub project_memory: &'static [MemoryFileSpec],
    pub global_memory: &'static [MemoryFileSpec],
}

pub const SHARED_PROJECT_FILES: &[&str] = &["AGENTS.md"];

pub const KNOWN_TOOLS: &[CodingTool] = &[
    CodingTool {
        name: "Claude Code",
        command: "claude",
        icon: IconName::AiClaude,
        binary_search_paths: &[
            ".local/bin/claude",
            ".bun/bin/claude",
            ".npm-global/bin/claude",
            ".volta/bin/claude",
        ],
        project_memory: &[
            MemoryFileSpec {
                pattern: "CLAUDE.md",
                is_dir: false,
                subdirs: &[],
            },
            MemoryFileSpec {
                pattern: ".claude/memory",
                is_dir: true,
                subdirs: &[],
            },
            MemoryFileSpec {
                pattern: ".claude/rules",
                is_dir: true,
                subdirs: &[],
            },
        ],
        global_memory: &[
            MemoryFileSpec {
                pattern: ".claude/CLAUDE.md",
                is_dir: false,
                subdirs: &[],
            },
            MemoryFileSpec {
                pattern: ".claude/rules",
                is_dir: true,
                subdirs: &[],
            },
        ],
    },
    CodingTool {
        name: "Codex",
        command: "codex",
        icon: IconName::AiOpenAi,
        binary_search_paths: &[".local/bin/codex"],
        project_memory: &[MemoryFileSpec {
            pattern: "CODEX.md",
            is_dir: false,
            subdirs: &[],
        }],
        global_memory: &[],

    },
    CodingTool {
        name: "Copilot CLI",
        command: "copilot",
        icon: IconName::Copilot,
        binary_search_paths: &[],
        project_memory: &[MemoryFileSpec {
            pattern: ".github/copilot-instructions.md",
            is_dir: false,
            subdirs: &[],
        }],
        global_memory: &[],

    },
    CodingTool {
        name: "OpenCode",
        command: "opencode",
        icon: IconName::AiOpenCode,
        binary_search_paths: &[".local/bin/opencode"],
        project_memory: &[MemoryFileSpec {
            pattern: "INSTRUCTIONS.md",
            is_dir: false,
            subdirs: &[],
        }],
        global_memory: &[],

    },
    CodingTool {
        name: "Aider",
        command: "aider",
        icon: IconName::Code,
        binary_search_paths: &[
            ".local/bin/aider",
            ".local/pipx/venvs/aider-chat/bin/aider",
        ],
        project_memory: &[
            MemoryFileSpec {
                pattern: ".aider.conf.yml",
                is_dir: false,
                subdirs: &[],
            },
            MemoryFileSpec {
                pattern: ".aiderignore",
                is_dir: false,
                subdirs: &[],
            },
        ],
        global_memory: &[MemoryFileSpec {
            pattern: ".aider.conf.yml",
            is_dir: false,
            subdirs: &[],
        }],

    },
    CodingTool {
        name: "Goose",
        command: "goose",
        icon: IconName::BoltFilled,
        binary_search_paths: &[".local/bin/goose"],
        project_memory: &[MemoryFileSpec {
            pattern: ".goosehints",
            is_dir: false,
            subdirs: &[],
        }],
        global_memory: &[],

    },
];

