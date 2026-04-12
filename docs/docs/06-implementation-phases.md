# Implementation Phases

## Phase 0: Fork and Build (1-2 days)

- [ ] Fork zed-industries/zed
- [ ] Set up build tooling (see Build Optimization below)
- [ ] Build successfully on macOS and Linux
- [ ] Run and verify baseline functionality
- [ ] Set up brosh_2 branch, CI if desired
- [ ] Document build dependencies for both platforms

### Build Optimization (Linux — MiniBook X / N150)

The N150 is a 4-core low-power chip. Rust builds are heavy. Set up these before your first build:

**1. Install mold linker**
```bash
sudo dnf install mold clang
```

**2. Configure cargo to use mold**

Zed already has a `.cargo/config.toml` with rustflags. Add mold to the project's config (not global, to preserve Zed's existing flags):

Add to the project's `.cargo/config.toml` under the linux target:
```toml
[target.x86_64-unknown-linux-gnu]
linker = "clang"
rustflags = ["-C", "link-arg=-fuse-ld=mold", "-C", "symbol-mangling-version=v0", "--cfg", "tokio_unstable"]
```

**Note:** Include Zed's existing rustflags (`symbol-mangling-version=v0`, `tokio_unstable`) since target-specific rustflags override global ones.

**Note:** Cranelift would help further but is nightly-only. Zed pins stable Rust via `rust-toolchain.toml`. Mold alone cuts link time by 50-70%, which is the biggest win on incremental builds.

**Milestone:** Unmodified Zed fork compiles and runs on both machines.

---

## Phase 1: Strip AI and Collaboration (the big cleanup)

**Goal:** Remove Zed's AI subscription, collaboration, and extension infrastructure.

- [ ] Wave 1: Remove agent_ui, agent, assistant, language_model, anthropic, open_ai, google_ai crates
- [ ] Wave 2: Remove collab, collab_ui, call, channel, rpc, live_kit crates
- [ ] Wave 3: Remove extension, extension_api, extension_host, extensions_ui crates
- [ ] Wave 4: Remove copilot, supermaven, inline_completion, outline_panel crates
- [ ] Remove Zed account/sign-in requirements
- [ ] Remove telemetry (or replace with opt-in local-only)
- [ ] Update `initialize_panels()` in zed.rs to remove stripped panels
- [ ] Fix all compilation errors from removed crates
- [ ] Test: app launches, terminals work, file browser works, git works, editor works with LSP

**Milestone:** Clean Zed fork with no AI/collab/extension overhead. Full editor with LSP intact. Compiles and runs.

---

## Phase 2: Terminal-First Experience

**Goal:** Make the terminal the primary experience.

- [ ] Change startup to open a center terminal tab (not welcome/editor)
- [ ] Simplify split: remove split-up and split-left actions
- [ ] Map `Cmd/Ctrl+D` → split right, `Cmd/Ctrl+Shift+D` → split down
- [ ] Terminal tabs show CWD or process name (not "Terminal 1")
- [ ] Remove breadcrumbs
- [ ] Simplify status bar (keep diagnostics, remove noise)
- [ ] Test: launch → terminal → split → navigate feels natural

**Milestone:** App launches into terminal. Splits are simple. Chrome is minimal.

---

## Phase 3: AI Agent Terminal Panel

**Goal:** Dedicated right-dock panel for running AI coding agents.

- [ ] Create `crates/ai_terminal_panel/` following the Panel trait pattern
- [ ] Panel contains a terminal view (reuse terminal infrastructure)
- [ ] Header shows: agent name/status, CWD
- [ ] Toggle with keyboard shortcut (e.g., `Cmd/Ctrl+Shift+A`)
- [ ] Persists across sessions (workspace serialization)
- [ ] Support multiple tabs (one per worktree/project)
- [ ] Register in `initialize_panels()`, position: right dock
- [ ] Add prominent toggle button

**Milestone:** Can launch Claude Code in the right panel while using terminal in center.

---

## Phase 4: Context Panel

**Goal:** Surface CLAUDE.md and context files for viewing/editing.

- [ ] Create `crates/context_panel/` following the Panel trait pattern
- [ ] Discover context files: CLAUDE.md, .claude/*, project docs
- [ ] Tree view listing discovered files
- [ ] Click to open in Zed's editor (as a tab in center pane)
- [ ] Toggle with keyboard shortcut (e.g., `Cmd/Ctrl+Shift+C`)
- [ ] Position: left dock, alongside project panel and git panel
- [ ] Add toggle button to dock strip

**Milestone:** Can browse and edit context/memory files from a dedicated panel.

---

## Phase 5: UI Polish

**Goal:** Make it feel like brosh, not like stripped Zed.

- [ ] Increase panel toggle icon sizes (24-28px)
- [ ] Add labels to panel toggle buttons
- [ ] Create/select default dark theme optimized for terminal
- [ ] Rebrand: app name "Brosh", custom icon
- [ ] Simplify command palette (filter to relevant commands)
- [ ] Simplify menus (remove editor-heavy entries)
- [ ] Clean up settings (remove settings for stripped features)
- [ ] Terminal-optimized keybinding defaults

**Milestone:** Looks and feels like a purpose-built terminal-first tool.

---

## Future Ideas (not in initial scope)

- Session recording (asciicast export from terminal)
- Terminal multiplexer features (named sessions, detach/reattach)
- Quick-open for recent terminals / directories
- Terminal-aware file opener (detect file paths in terminal output, click to open)
- Custom status bar widgets (battery, network, etc.)
- Theming that unifies terminal ANSI colors with UI chrome
