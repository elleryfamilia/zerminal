//! Copilot CLI `/ide` integration.
//!
//! Implements the IDE side of the protocol GitHub Copilot CLI uses to
//! discover and connect to a host editor (the same protocol VS Code's
//! Copilot Chat extension implements). Auto-connect is lockfile-driven:
//! the CLI scans the state directory and picks a lockfile whose
//! `workspaceFolders` covers its current working directory.
//!
//! Authoritative protocol reference: `microsoft/vscode-copilot-chat`,
//! `src/extension/chatSessions/copilotcli/` (MIT-licensed).

pub mod broadcaster;
pub mod lockfile;
pub mod mcp;
pub mod transport;

pub use broadcaster::Broadcaster;
pub use lockfile::{Lockfile, LockfileGuard, copilot_state_dir, sweep_stale, write_atomic};
pub use mcp::{McpDispatcher, McpPostHandler, ToolCall, ToolCallSender};
