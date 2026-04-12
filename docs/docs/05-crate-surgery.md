# Crate Surgery — What to Remove from Zed

## Goal

Strip Zed down to the terminal-first core. Remove crates that serve editor-centric, collaboration, AI subscription, or extension workflows. This reduces build time, binary size, and maintenance burden.

## Removal Strategy

Remove in waves. Each wave should compile and run before moving to the next.

---

## Wave 1: AI / Agent System (highest priority — removes Zed's AI subscription dependency)

| Crate | Purpose | Why Remove |
|-------|---------|------------|
| `agent_ui` | AI agent chat panel UI | Replaced by ai_terminal_panel |
| `agent` | Agent infrastructure | Not needed |
| `assistant*` | Assistant-related crates | Zed's built-in AI |
| `language_model*` | LLM provider integrations | We use CLI agents, not API |
| `prompt_library` | Prompt management | Zed AI feature |
| `anthropic` | Anthropic API client | Not needed |
| `open_ai` | OpenAI API client | Not needed |
| `google_ai` | Google AI client | Not needed |

**Impact:** Removes Zed account/subscription requirements. Large code reduction.

---

## Wave 2: Collaboration

| Crate | Purpose | Why Remove |
|-------|---------|------------|
| `collab` | Collaboration server/client | Not needed |
| `collab_ui` | Collaboration UI | Not needed |
| `call` | Audio/video calls | Not needed |
| `channel` | Chat channels | Not needed |
| `rpc` | RPC protocol for collab | Not needed |
| `live_kit*` | Video infrastructure | Not needed |

**Impact:** Removes network dependencies, simplifies auth.

---

## Wave 3: Extensions

| Crate | Purpose | Why Remove |
|-------|---------|------------|
| `extension` | Extension host (WASM) | We fork, not extend |
| `extension_api` | Extension API definitions | Not needed |
| `extension_host` | Extension runtime | Not needed |
| `extensions_ui` | Extension marketplace | Not needed |

**Impact:** Removes WASM runtime dependency.

---

## Wave 4: AI Integrations and Unnecessary Editor Extras

| Crate | Purpose | Why Remove |
|-------|---------|------------|
| `copilot` | GitHub Copilot integration | We use CLI agents, not inline completion |
| `supermaven*` | Supermaven integration | Same — not needed |
| `inline_completion*` | Inline AI completions | Same — not needed |
| `outline_panel` | Symbol outline panel | Low value for terminal-first workflow |

**Keep these editor features:**

| Crate | Purpose | Why Keep |
|-------|---------|----------|
| `lsp` | Language Server Protocol | Full editor functionality — go-to-definition, hover, completions |
| `diagnostics` | Error/warning diagnostics | Useful when reviewing AI-generated code |
| `debugger_ui` / `dap` | Debugger | Useful for debugging — evaluate later |
| `tasks` | Task runner | Evaluate later — may be useful alongside terminal |
| `vim` | Vim mode | Keep — low maintenance, high value |

**Impact:** Smaller than original Wave 4 plan, but avoids the risky editor+LSP untangling.

---

## Wave 5: Evaluate / Maybe Remove

| Crate | Purpose | Decision |
|-------|---------|----------|
| `search` | Project-wide search | KEEP — useful for reviewing code |
| `language` | Tree-sitter grammars | KEEP — syntax highlighting still valuable |
| `markdown` | Markdown rendering | KEEP — useful for context panel |
| `vim` | Vim mode | KEEP if low maintenance burden |
| `auto_update` | Auto-updater | REPLACE with own update mechanism |
| `telemetry` | Zed telemetry | REMOVE or replace |
| `client` | Zed cloud client | REMOVE — no Zed account needed |

---

## Crates to Definitely Keep

| Crate | Purpose |
|-------|---------|
| `gpui` | UI framework — the foundation |
| `ui` | Component library |
| `theme` | Theme system |
| `settings` | Settings infrastructure |
| `workspace` | Workspace/pane/dock management |
| `terminal` | Terminal core |
| `terminal_view` | Terminal UI |
| `project_panel` | File browser |
| `git_ui` | Git panel |
| `editor` | Code editor (full functionality) |
| `language` | Syntax highlighting |
| `lsp` | Language server support |
| `diagnostics` | Error/warning panel |
| `vim` | Vim keybinding mode |
| `project` | Project/worktree management |
| `fs` | Filesystem abstraction |
| `db` | Local database (workspace persistence) |
| `command_palette` | Command palette |
| `search` | Search functionality |
| `markdown` | Markdown rendering |
| `welcome` | Rework into terminal-first welcome |

---

## New Crates to Add

| Crate | Purpose |
|-------|---------|
| `ai_terminal_panel` | Right dock panel with terminal for AI agents |
| `context_panel` | Left dock panel for CLAUDE.md and context files |

---

## Build Impact Estimate

Zed full build: ~200+ crates, several minutes.
After surgery: ~80-100 crates. Faster builds, smaller binary, fewer dependencies.

## Notes

- Each wave should be a separate branch/PR in the fork
- Test compilation and basic functionality after each wave
- Some crates are deeply intertwined (editor + lsp especially) — Wave 4 will be the hardest
- Keep git history clean to make rebasing on upstream Zed easier (if desired)
