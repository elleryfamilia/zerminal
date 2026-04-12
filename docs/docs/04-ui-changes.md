# UI Changes — Brosh 2

## Guiding Principles

- Larger touch/click targets — optimized for quick toggling, not pixel hunting
- Fewer options per interaction — simplify splits, menus, actions
- Terminal-first visual hierarchy — terminal panes should dominate
- Reduce chrome that serves editor-centric workflows

---

## 1. Panel Toggle Buttons (Left Dock)

**Problem:** Zed's panel toggle icons are small (16-20px), tightly packed in the sidebar strip.

**Change:**
- Increase icon size to 24-28px
- Add more padding between icons
- Add text labels below icons (or on hover)
- Consider a vertical button strip with icon + short label (like "Files", "Git", "Context")
- Active panel indicator should be bold/obvious, not a subtle highlight

**Where:** `crates/workspace/src/dock.rs`, panel icon rendering. Also `crates/ui/` icon components.

---

## 2. Split Behavior

**Problem:** Zed offers 4-way splits (up, down, left, right) via menus and keybindings.

**Change:**
- Default split action splits **right**
- `Cmd/Ctrl+D` or a single "split" button → split right
- `Cmd/Ctrl+Shift+D` → split down
- Remove split-up and split-left from UI and keybindings
- No split direction picker popup — one key = one action

**Where:** `crates/workspace/src/pane.rs`, split actions. Keybinding definitions in `assets/keymaps/`.

---

## 3. Terminal-First Startup

**Problem:** Zed opens to a welcome tab or last editor session.

**Change:**
- On fresh launch: open a single center terminal tab
- On session restore: restore layout but ensure at least one terminal tab exists
- No welcome tab, no "getting started" experience
- First-run experience: just a terminal

**Where:** `crates/zed/src/zed.rs` initialization, `crates/workspace/` session restore logic.

---

## 4. Simplified Tab Bar

**Problem:** Tab bar has many small controls (split button dropdown, close buttons, overflow menu, breadcrumbs).

**Change:**
- Remove breadcrumbs (file path indicator below tabs) — useful for deep codebases, not for terminal workflow
- Simplify tab close — single X button, no dropdown
- Terminal tabs show shell name or CWD, not "Terminal 1, Terminal 2"
- Editor tabs show filename only (not path)
- Visual distinction between terminal tabs and editor tabs (subtle icon or color)

**Where:** `crates/workspace/src/pane.rs` tab bar rendering, `crates/terminal_view/` tab title.

---

## 5. Status Bar

**Problem:** Zed's status bar is dense with editor-centric info (language, line/col, diagnostics count, copilot status).

**Change:**
- Remove: language indicator, line/col position, diagnostics count, copilot/AI status
- Keep: git branch, notification indicators
- Add: active shell indicator, AI agent status (running/idle), current working directory
- Larger text, more breathing room

**Where:** `crates/workspace/src/status_bar.rs`, individual status bar item crates.

---

## 6. Right Dock — AI Agent Panel

**Problem:** Zed's right dock hosts the agent_ui (their AI chat). We're replacing it.

**Change:**
- New `ai_terminal_panel` crate — a dock panel containing a terminal running Claude Code, Codex, or any CLI agent
- Simple UI: terminal + header showing agent name/status
- Toggle with a prominent button and keyboard shortcut
- Panel persists across workspace sessions
- Can hold multiple agent tabs (one per worktree or project)

**Where:** New crate `crates/ai_terminal_panel/`. Registered in `initialize_panels()`.

---

## 7. Context Panel

**Problem:** No equivalent in Zed. New feature carried from brosh v1.

**Change:**
- New `context_panel` crate — left dock panel
- Shows CLAUDE.md, .claude/ memory files, project context files
- Uses Zed's native editor (not TipTap) for viewing/editing
- Tree view of discovered context files
- Toggle alongside file browser and git panel

**Where:** New crate `crates/context_panel/`. Registered in `initialize_panels()`.

---

## 8. Command Palette

**Problem:** Zed's command palette has hundreds of commands, most editor-centric.

**Change:**
- Filter command palette to relevant commands only
- Add brosh-specific commands: "Open AI Agent", "Toggle Context Panel", "New Terminal Right", "New Terminal Down"
- Remove or hide LSP, collaboration, extension, and AI assistant commands

**Where:** Command registration throughout crates. Filter in `crates/command_palette/`.

---

## 9. Menus

**Problem:** Zed's menus (application menu, context menus) are editor-heavy.

**Change:**
- Simplify application menu — remove collaboration, extensions, AI assistant entries
- Terminal context menu: copy, paste, clear, split right, split down
- File browser context menu: open, open in terminal, copy path, delete (keep it minimal)

**Where:** `crates/zed/src/main.rs` menu definitions, individual crate context menus.

---

## 10. Color / Visual Identity

**Problem:** Zed looks like Zed.

**Change:**
- Default theme: dark, terminal-optimized (high contrast for terminal text, muted chrome)
- Rename app to "Brosh" throughout
- Custom app icon
- Consider carrying over brosh v1 themes (Catppuccin, Dracula, Nord, etc. — most already exist as Zed themes)

**Where:** `crates/theme/`, `assets/`, app metadata/branding files.
