# Brosh v1 — Feature Audit

Current implementation: Electron + React + xterm.js + Monaco
Repo: https://github.com/elleryfamilia/brosh

## Features — Keep, Adapt, or Drop

### KEEP (core to the workflow)

| Feature | Current Implementation | Brosh 2 Approach |
|---------|----------------------|------------------|
| Split terminal panes | xterm.js + pane tree data structure | Zed center terminal splits (right/down only) |
| Multi-tab terminals | React tabs per pane | Zed tab system (terminals and editors share tabs) |
| Claude/AI agent pane | Dedicated right panel, multi-tab per worktree | Right dock panel, persistent terminal |
| File browser | Plugin sidebar, tree view, material icons | Zed project_panel (simplified) |
| Git panel | Plugin sidebar, commit graph, diff viewer | Zed git_ui (simplified) |
| Context/memory panel | Plugin sidebar, CLAUDE.md viewer, TipTap editor | New panel crate, simpler editor (Zed's native editor) |
| Editor for file viewing | Monaco with vim mode | Zed's editor (far superior) |
| Diff viewing | Monaco diff view via git plugin | Zed's built-in diff |
| Theme support | 9 themes, ANSI + UI colors | Zed theme system (hundreds of themes) |
| Session persistence | electron-store, localStorage | Zed workspace serialization |

### ADAPT (carry the concept, change implementation)

| Feature | Current Implementation | Brosh 2 Approach |
|---------|----------------------|------------------|
| Plugin sidebar host | Generic plugin system, self-registration, SidebarHost | Zed dock panels (left dock with project/git/context panels) |
| Plugin badges/status | Status bar badges per plugin | Simplified status bar indicators |
| Worktree detection | Claude panel detects git worktrees | Can leverage Zed's project/worktree awareness |
| OSC shell integration | OSC 133 parsing for prompt/command/output marks | Zed terminal already handles this |
| IDE protocol | ide-protocol.ts for opening files from terminal | Zed already handles `code` / editor protocol URIs |

### DROP (redundant or low-value in Zed context)

| Feature | Reason to Drop |
|---------|---------------|
| NL detection / ML classifier | Zed isn't routing input — user explicitly uses terminal or AI pane |
| Typo correction | Low value, adds complexity |
| MCP server for terminal control | Claude Code has its own terminal; Zed has MCP context server support |
| Sandbox mode | Claude Code has built-in sandboxing now |
| Session recording (asciicast) | Nice-to-have, not core. Add later if needed |
| Markdown terminal rendering | Claude Code handles its own output rendering |
| Port detection badge | Low value |
| Feedback badge | Not needed for personal/OSS tool |
| PostHog analytics | Not needed |
| Auto-updater | Replace with simple git-based updates or GitHub releases |
| Bytecode compilation | Not applicable (Rust binary) |
| 25+ bundled fonts | Zed has its own font system |
| HuggingFace model bundling | Dropped with NL detection |

## Architecture Comparison

### Brosh v1 (Electron)
```
Renderer (React)
  ├── Terminal Panes (xterm.js + WebGL)
  ├── Claude Panel (xterm.js)
  ├── Plugin Sidebar Host
  │   ├── Files Plugin (React tree view)
  │   ├── Git Plugin (React + Monaco diff)
  │   ├── Context Plugin (React + TipTap)
  │   └── Plans Plugin (React + TipTap)
  ├── Editor Pane (Monaco)
  └── Status Bar (React)
Main Process (Node.js)
  ├── Terminal Bridge (node-pty)
  ├── MCP Server (Unix socket)
  ├── AI Detection (ML classifier)
  └── Settings/Persistence
```

### Brosh 2 (Zed fork)
```
GPUI Window
  ├── Center Workspace
  │   ├── Terminal tabs (alacritty_terminal)
  │   └── Editor tabs (Zed editor, Tree-sitter)
  ├── Left Dock
  │   ├── Project Panel (file browser)
  │   ├── Git Panel
  │   └── Context Panel (new crate)
  ├── Right Dock
  │   └── AI Agent Panel (terminal-based, new crate)
  └── Status Bar
```
