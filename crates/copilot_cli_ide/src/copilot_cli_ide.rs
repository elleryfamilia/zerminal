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
//!
//! Unix-only: the transport binds a Unix domain socket. Windows would need a
//! named-pipe variant; not built until Zerminal ships on Windows.

#![cfg(unix)]

mod attachment;
mod broadcaster;
mod lockfile;
mod mcp;
mod transport;

pub use attachment::CopilotAttachment;
