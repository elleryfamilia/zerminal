# Zed Architecture — Reference for Forking

## Repository

https://github.com/zed-industries/zed

## Tech Stack

- Language: Rust
- UI Framework: GPUI (custom, GPU-accelerated, in-repo)
- Rendering: Metal (macOS), Vulkan via Blade (Linux)
- Terminal: alacritty_terminal crate (embedded)
- Editor: Custom, Tree-sitter for syntax, Rope data structure
- Build: Cargo workspace, ~200+ crates

## Crate Structure (relevant crates)

### Core Framework
- `crates/gpui/` — GPU-accelerated UI framework. Retained-mode, Rust-native. Elements, views, windows, events, styling.
- `crates/ui/` — Component library built on GPUI. Buttons, labels, icons, lists, tabs, tooltips, popovers.
- `crates/theme/` — Theme system. Colors, syntax highlighting, UI element styles.
- `crates/settings/` — Settings infrastructure. JSON config, defaults, schema.
- `crates/workspace/` — Workspace management. Panes, docks, panels, items, serialization.

### Terminal
- `crates/terminal/` — Terminal core. Wraps alacritty_terminal. PTY management, input handling, ANSI parsing.
- `crates/terminal_view/` — Terminal UI. `TerminalView` (renders terminal in a pane), `TerminalPanel` (dock panel with splits).

### Editor
- `crates/editor/` — Code editor. Buffer management, selections, input handling, display map.
- `crates/language/` — Language support. Tree-sitter grammars, syntax highlighting, language config.
- `crates/multi_buffer/` — Multi-buffer support for editor.

### Panels (the pattern we'll follow)
- `crates/project_panel/` — File browser. Tree view, file operations, drag-drop.
- `crates/git_ui/` — Git panel. Status, diff, commit, branch operations.
- `crates/outline_panel/` — Symbol outline. (candidate for removal)
- `crates/agent_ui/` — AI agent panel. Chat, tools, MCP. (removing this)

### Infrastructure (candidates for removal)
- `crates/collab/` — Collaboration server/client
- `crates/call/` — Audio/video calls
- `crates/channel/` — Chat channels
- `crates/lsp/` — Language Server Protocol client
- `crates/extensions_ui/` — Extension marketplace UI
- `crates/extension/` — Extension host (WASM runtime)
- `crates/assistant*/` — Various assistant-related crates
- `crates/agent/` — Agent infrastructure

## Panel Trait

Location: `crates/workspace/src/dock.rs`

Every dock panel implements:
```rust
pub trait Panel: Focusable + EventEmitter<PanelEvent> + Render {
    fn persistent_name() -> &'static str;
    fn position(&self, cx: &App) -> DockPosition;       // Left, Right, Bottom
    fn set_position(&mut self, position: DockPosition, cx: &mut App);
    fn default_size(&self, cx: &App) -> Pixels;
    fn icon(&self, cx: &App) -> Option<IconName>;
    fn icon_tooltip(&self, cx: &App) -> Option<SharedString>;
    fn toggle_action(&self) -> Box<dyn Action>;
    // ... additional optional methods
}
```

## Panel Registration

Location: `crates/zed/src/zed.rs` in `initialize_panels()`

```rust
workspace.add_panel::<ProjectPanel>(window, cx);
workspace.add_panel::<GitPanel>(window, cx);
workspace.add_panel::<TerminalPanel>(window, cx);
// ... etc
```

## GPUI Basics

UI is built with Rust macros and a styled element system:
```rust
div()
    .flex()
    .flex_col()
    .size_full()
    .bg(cx.theme().colors().background)
    .child(self.render_header(cx))
    .child(self.render_content(cx))
```

Key concepts:
- `View<T>` — a handle to a rendered component with state
- `Render` trait — implement to make something renderable
- `div()` — base element (like HTML div)
- Flexbox layout (same model as CSS flexbox)
- `cx: &mut Context<Self>` — context for state updates, subscriptions, async

## Terminal Architecture

`TerminalView` wraps `Terminal` which wraps `alacritty_terminal::Term`.

Key files:
- `crates/terminal/src/terminal.rs` — core terminal logic, PTY spawning, input/output
- `crates/terminal_view/src/terminal_view.rs` — renders terminal as an `Item` in workspace panes
- `crates/terminal_view/src/terminal_panel.rs` — dock panel containing terminal pane group

Terminals can exist in:
1. Center workspace (as `Item` tabs, via `NewCenterTerminal`)
2. Terminal panel dock (as panel with its own splits)

## Key Actions / Entry Points

- `workspace::NewTerminal` — opens terminal
- `workspace::NewCenterTerminal` — opens terminal in center pane
- `terminal_panel::ToggleFocus` — toggle/focus terminal panel
- `project_panel::ToggleFocus` — toggle/focus file browser
- `git_panel::ToggleFocus` — toggle/focus git panel

## Build

```bash
# macOS
cargo build --release

# Linux (needs specific deps)
# See script/linux for dependency installation
cargo build --release
```

## Config

User config at `~/.config/zed/settings.json` and `~/.config/zed/keymap.json`.
