//! Claude Code `/ide` integration.
//!
//! Hosts a localhost WebSocket server that the `claude` CLI auto-connects to
//! when launched in a Zerminal terminal pane with `CLAUDE_CODE_SSE_PORT` and
//! `ENABLE_IDE_INTEGRATION=true` in its env. Serves MCP tool calls
//! (openFile, getCurrentSelection, getOpenEditors, etc.) by delegating to the
//! `EditorCapabilities` trait, and pushes selection-change notifications back
//! to the CLI.
//!
//! See:
//!   - <https://github.com/coder/claudecode.nvim/blob/main/PROTOCOL.md>
//!   - <https://github.com/coder/claudecode.nvim/blob/main/ARCHITECTURE.md>

mod attachment;
mod broadcaster;
mod lockfile;
mod mcp;
mod server;

pub use attachment::{AttachmentState, ClaudeCodeAttachment};
pub use broadcaster::Broadcaster;
pub use lockfile::{Lockfile, LockfileGuard, sweep_stale_lockfiles};
