# Agent Launch Wrapper Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let an external launcher program (e.g. loadout's `load run <agent>`) wrap AI agent launches in the AI terminal panel, configured via a string template in settings and surfaced in the settings dialog and panel UI.

**Architecture:** A new optional `ai_terminal.launcher` settings object (string template + enabled flag + allowlist) flows through `settings_content` into the resolved `AiTerminalSettings`. One pure function in `agent_detection.rs` (`wrapped_launch`) turns an `AiAgent` plus the launcher config into the actual `(program, args)` to spawn; `spawn_agent` calls it when building `SpawnInTerminal`. Everything else — agent identity, `/ide` attachments, env injection — is untouched. UI: a "Coding Tools" section in the settings dialog, a "Configure Launch Wrapper…" menu entry, and "via load"-style hints on wrapped agents.

**Tech Stack:** Rust, GPUI, Zed settings system (`settings_content` / `settings` / `settings_ui`), `shlex` for shell-splitting (already a workspace dependency).

**Spec:** `docs/superpowers/specs/2026-07-31-agent-launch-wrapper-design.md`

## Global Constraints

- Branch: work on `agent-launch-wrapper` (already created; never commit to `main`).
- Conventional Commits, imperative subject ≤72 chars.
- No `unwrap()`/panicking indexing; propagate or log errors (`.log_err()` / `log::warn!`).
- Comments only for non-obvious "why", never restating code.
- The wrapper is an enhancement, never a gate: malformed config ⇒ raw launch with `log::warn!`; a configured-but-missing wrapper binary ⇒ visible spawn error in the tab (do NOT pre-validate existence).
- Placeholders: `{agent}` = zerminal agent id; `{command}` = resolved agent executable path. Template with neither placeholder = pure prefix (agent executable appended).
- Agent's own `args` are always appended after the substituted template.
- This machine is a 4-core Intel N150 on battery: always build with `-j2`, prefer `cargo check`/targeted `cargo test` while iterating. `./script/clippy` builds in `--release` — run it once at the end, not per task.

---

### Task 1: Settings schema (`settings_content`) + defaults

**Files:**
- Modify: `crates/settings_content/src/ai_terminal.rs`
- Modify: `assets/settings/default.json` (~line 2559-2570, the `ai_terminal` block)

**Interfaces:**
- Produces: `AiTerminalLauncherSettings { enabled: Option<bool>, command: Option<String>, agents: Option<Vec<String>> }` and field `launcher: Option<AiTerminalLauncherSettings>` on `AiTerminalSettingsContent`. Task 2 reads these in `AiTerminalSettings::from_settings`; Task 5 reads/writes them from the settings dialog.

- [ ] **Step 1: Add the launcher settings struct**

In `crates/settings_content/src/ai_terminal.rs`, add to `AiTerminalSettingsContent`:

```rust
    /// Wraps agent launches in an external launcher command (e.g. a context
    /// renderer like `load run claude`, `distrobox enter dev --`, or an env
    /// shim). Applies to every agent unless `launcher.agents` narrows it.
    pub launcher: Option<AiTerminalLauncherSettings>,
```

and append the new struct at the end of the file:

```rust
/// Wraps AI agent launches in an external launcher command.
#[with_fallible_options]
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema, MergeFrom)]
pub struct AiTerminalLauncherSettings {
    /// Whether agent launches are wrapped when `command` is set.
    ///
    /// Default: true
    pub enabled: Option<bool>,
    /// Wrapper command template, shell-split at launch time (quotes
    /// respected). `{agent}` is replaced with the agent id ("claude",
    /// "codex", …) and `{command}` with the resolved agent executable path.
    /// A template with neither placeholder is used as a prefix: the agent
    /// executable is appended after it. The agent's own arguments are always
    /// appended last. Empty or unset disables wrapping.
    ///
    /// Default: ""
    pub command: Option<String>,
    /// Agent ids the wrapper applies to. Unset applies it to all agents.
    pub agents: Option<Vec<String>>,
}
```

- [ ] **Step 2: Add defaults to `assets/settings/default.json`**

Replace the line `"ai_terminal": { "agents": {} },` with:

```json
  "ai_terminal": {
    "agents": {},
    // Wraps agent launches in an external launcher command. "command" is a
    // template split like a shell command line: {agent} becomes the agent id,
    // {command} the agent executable; with neither placeholder the template
    // is a prefix. Optional "agents" narrows which agent ids are wrapped.
    // Example: "launcher": { "command": "load run {agent} --" }
    "launcher": { "enabled": true, "command": "" }
  },
```

(Keep the existing comment block above the key that documents `"agents"`.)

- [ ] **Step 3: Verify it compiles and defaults parse**

Run: `cargo check -p settings_content -j2 && cargo test -p settings -j2`
Expected: check passes; the settings crate's default-settings parse tests pass. If a test asserts every default key round-trips, the new `launcher` defaults are covered by the JSON added in Step 2.

- [ ] **Step 4: Commit**

```bash
git add crates/settings_content/src/ai_terminal.rs assets/settings/default.json
git commit -m "feat: add ai_terminal.launcher settings schema"
```

---

### Task 2: Resolved `LauncherConfig` + launch functions (TDD)

**Files:**
- Modify: `crates/ai_terminal_panel/Cargo.toml` (add `shlex.workspace = true` to `[dependencies]`)
- Modify: `crates/ai_terminal_panel/src/agent_detection.rs`

**Interfaces:**
- Consumes: `AiTerminalLauncherSettings` from Task 1 (via `SettingsContent.ai_terminal`).
- Produces (used by Tasks 3 and 4):
  - `pub struct LauncherConfig { pub enabled: bool, pub command: String, pub agents: Option<Vec<String>> }`
  - field `pub launcher: Option<LauncherConfig>` on the resolved `AiTerminalSettings`
  - `pub fn wrapped_launch(agent: &AiAgent, launcher: Option<&LauncherConfig>) -> Option<(PathBuf, Vec<String>)>`
  - `pub fn launch_command(agent: &AiAgent, launcher: Option<&LauncherConfig>) -> (PathBuf, Vec<String>)`
  - `pub fn launcher_hint(agent: &AiAgent, launcher: Option<&LauncherConfig>) -> Option<String>` (returns e.g. `"via load"`)

- [ ] **Step 1: Add the shlex dependency**

In `crates/ai_terminal_panel/Cargo.toml` `[dependencies]` (keep alphabetical order, after `serde_json`):

```toml
shlex.workspace = true
```

- [ ] **Step 2: Write the failing tests**

Append to the `tests` module in `agent_detection.rs`:

```rust
    fn wrapper_test_agent() -> AiAgent {
        AiAgent {
            id: "claude".to_string(),
            name: "Claude Code".to_string(),
            args: vec!["--model".to_string(), "opus".to_string()],
            env: HashMap::default(),
            icon: IconName::AiClaude,
            path: PathBuf::from("/usr/bin/claude"),
        }
    }

    fn launcher(command: &str) -> LauncherConfig {
        LauncherConfig {
            enabled: true,
            command: command.to_string(),
            agents: None,
        }
    }

    #[test]
    fn no_launcher_launches_raw() {
        let agent = wrapper_test_agent();
        let (program, args) = launch_command(&agent, None);
        assert_eq!(program, PathBuf::from("/usr/bin/claude"));
        assert_eq!(args, agent.args);
    }

    #[test]
    fn agent_placeholder_is_substituted() {
        let agent = wrapper_test_agent();
        // Absolute wrapper path: find_in_path falls back to the literal for
        // paths that don't exist, keeping this test machine-independent.
        let launcher = launcher("/opt/wrap/load run {agent} --");
        let (program, args) = launch_command(&agent, Some(&launcher));
        assert_eq!(program, PathBuf::from("/opt/wrap/load"));
        assert_eq!(args, vec!["run", "claude", "--", "--model", "opus"]);
    }

    #[test]
    fn command_placeholder_is_substituted() {
        let agent = wrapper_test_agent();
        let launcher = launcher("/opt/wrap/sandbox {command}");
        let (_, args) = launch_command(&agent, Some(&launcher));
        assert_eq!(args, vec!["/usr/bin/claude", "--model", "opus"]);
    }

    #[test]
    fn prefix_template_appends_agent_command() {
        let agent = wrapper_test_agent();
        let launcher = launcher("/opt/wrap/env FOO=1");
        let (program, args) = launch_command(&agent, Some(&launcher));
        assert_eq!(program, PathBuf::from("/opt/wrap/env"));
        assert_eq!(args, vec!["FOO=1", "/usr/bin/claude", "--model", "opus"]);
    }

    #[test]
    fn allowlist_filters_agents() {
        let agent = wrapper_test_agent();
        let mut config = launcher("/opt/wrap/load run {agent}");
        config.agents = Some(vec!["codex".to_string()]);
        assert!(wrapped_launch(&agent, Some(&config)).is_none());
        config.agents = Some(vec!["claude".to_string()]);
        assert!(wrapped_launch(&agent, Some(&config)).is_some());
    }

    #[test]
    fn disabled_or_empty_launcher_is_raw() {
        let agent = wrapper_test_agent();
        let mut config = launcher("/opt/wrap/load run {agent}");
        config.enabled = false;
        assert!(wrapped_launch(&agent, Some(&config)).is_none());
        assert!(wrapped_launch(&agent, Some(&launcher(""))).is_none());
        assert!(wrapped_launch(&agent, Some(&launcher("   "))).is_none());
    }

    #[test]
    fn malformed_template_is_raw() {
        let agent = wrapper_test_agent();
        let config = launcher("/opt/wrap/load \"unclosed");
        assert!(wrapped_launch(&agent, Some(&config)).is_none());
    }

    #[test]
    fn quoted_segments_survive_splitting() {
        let agent = wrapper_test_agent();
        let config = launcher("\"/opt/my wrap/load\" run {agent}");
        let (program, args) = launch_command(&agent, Some(&config));
        assert_eq!(program, PathBuf::from("/opt/my wrap/load"));
        assert_eq!(args, vec!["run", "claude", "--model", "opus"]);
    }

    #[test]
    fn launcher_hint_uses_program_name() {
        let agent = wrapper_test_agent();
        let config = launcher("/opt/wrap/load run {agent} --");
        assert_eq!(
            launcher_hint(&agent, Some(&config)),
            Some("via load".to_string())
        );
        assert_eq!(launcher_hint(&agent, None), None);
    }
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p ai_terminal_panel -j2`
Expected: FAIL to compile — `LauncherConfig`, `wrapped_launch`, `launch_command`, `launcher_hint` not found.

- [ ] **Step 4: Implement**

In `agent_detection.rs`, add after `AiTerminalAgentConfig`:

```rust
/// Resolved launch-wrapper configuration (see `AiTerminalLauncherSettings`).
#[derive(Clone, Debug, PartialEq)]
pub struct LauncherConfig {
    pub enabled: bool,
    pub command: String,
    pub agents: Option<Vec<String>>,
}
```

Add the field to the resolved settings struct:

```rust
pub struct AiTerminalSettings {
    pub agents: BTreeMap<String, AiTerminalAgentConfig>,
    pub launcher: Option<LauncherConfig>,
}
```

In `AiTerminalSettings::from_settings`, resolve it (after the `agents` mapping, before `Self { agents }` — the struct literal becomes `Self { agents, launcher }`):

```rust
        let launcher = content.launcher.map(|launcher| LauncherConfig {
            enabled: launcher.enabled.unwrap_or(true),
            command: launcher.command.unwrap_or_default(),
            agents: launcher.agents,
        });
```

Add the three functions after `detect_agents`:

```rust
/// The wrapped `(program, args)` for launching `agent` through the configured
/// launcher, or `None` when the launch should be raw: no launcher, disabled,
/// empty or malformed template, or the agent filtered out by the allowlist.
/// The wrapper is an enhancement, never a gate.
pub fn wrapped_launch(
    agent: &AiAgent,
    launcher: Option<&LauncherConfig>,
) -> Option<(PathBuf, Vec<String>)> {
    let launcher = launcher?;
    if !launcher.enabled || launcher.command.trim().is_empty() {
        return None;
    }
    if let Some(allowlist) = &launcher.agents
        && !allowlist.iter().any(|id| id == &agent.id)
    {
        return None;
    }
    let Some(template) = shlex::split(&launcher.command) else {
        log::warn!(
            "ai_terminal launcher template {:?} failed to parse; launching {:?} raw",
            launcher.command,
            agent.id
        );
        return None;
    };
    if template.is_empty() {
        return None;
    }

    let has_placeholder = template
        .iter()
        .any(|part| part.contains("{agent}") || part.contains("{command}"));
    let agent_command = agent.path.to_string_lossy();
    let mut argv: Vec<String> = template
        .iter()
        .map(|part| {
            part.replace("{agent}", &agent.id)
                .replace("{command}", &agent_command)
        })
        .collect();
    if !has_placeholder {
        argv.push(agent_command.to_string());
    }
    argv.extend(agent.args.iter().cloned());

    let program = argv.remove(0);
    let path = find_in_path(&program, &[]).unwrap_or_else(|| PathBuf::from(&program));
    Some((path, argv))
}

/// The `(program, args)` actually spawned for `agent`: the wrapped launch
/// when the launcher applies, the agent's own command otherwise.
pub fn launch_command(
    agent: &AiAgent,
    launcher: Option<&LauncherConfig>,
) -> (PathBuf, Vec<String>) {
    wrapped_launch(agent, launcher)
        .unwrap_or_else(|| (agent.path.clone(), agent.args.clone()))
}

/// Short hint for wrapped launches ("via load"), derived from the wrapper
/// program's file name. `None` when the launch is raw.
pub fn launcher_hint(agent: &AiAgent, launcher: Option<&LauncherConfig>) -> Option<String> {
    let (path, _) = wrapped_launch(agent, launcher)?;
    let name = path.file_name()?.to_string_lossy();
    Some(format!("via {name}"))
}
```

Note: `argv.remove(0)` cannot panic — the empty-template case returns `None` above, and the no-placeholder branch pushes the agent command, so `argv` is non-empty. `find_in_path(program, &[])` mirrors `resolve_command`'s semantics: absolute paths pass through `Path::join` unchanged, bare names are looked up in the system paths, and a miss falls back to the literal so the spawn surfaces a clear "not found" in the terminal tab.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p ai_terminal_panel -j2`
Expected: all tests PASS, including the pre-existing detection tests.

- [ ] **Step 6: Commit**

```bash
git add crates/ai_terminal_panel/Cargo.toml crates/ai_terminal_panel/src/agent_detection.rs
git commit -m "feat: resolve launcher config and compute wrapped launch commands"
```

---

### Task 3: Wire `launch_command` into `spawn_agent`

**Files:**
- Modify: `crates/ai_terminal_panel/src/ai_terminal_panel.rs` (import at line 9; `spawn_agent`, ~line 674)

**Interfaces:**
- Consumes: `launch_command`, `AiTerminalSettings::try_get` from Task 2.
- Produces: wrapped launches end-to-end (spawned terminal runs the wrapper).

- [ ] **Step 1: Extend the import**

Line 9 becomes:

```rust
use agent_detection::{AiAgent, AiTerminalSettings, detect_agents, launch_command, launcher_hint};
```

(`launcher_hint` is consumed in Task 4; including it here keeps this line stable. If clippy's unused-import lint fires between tasks, add it in Task 4 instead.)

- [ ] **Step 2: Use the wrapped command in `spawn_agent`**

In `spawn_agent`, immediately before the `let spawn_task = SpawnInTerminal { … }` construction, add:

```rust
        let launcher = AiTerminalSettings::try_get(cx).and_then(|settings| settings.launcher.clone());
        let (launch_program, launch_args) = launch_command(agent, launcher.as_ref());
```

and change the two fields in the `SpawnInTerminal` literal:

```rust
            command: Some(launch_program.to_string_lossy().to_string()),
            args: launch_args,
```

(They currently read `command: Some(agent.path.to_string_lossy().to_string())` and `args: agent.args.clone()`.)

Nothing else in `spawn_agent` changes: `agent.id` still gates `/ide` attachments, `env` handling is independent of the command line.

- [ ] **Step 3: Verify**

Run: `cargo check -p ai_terminal_panel -j2 && cargo test -p ai_terminal_panel -j2`
Expected: clean check, all tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/ai_terminal_panel/src/ai_terminal_panel.rs
git commit -m "feat: launch AI agents through the configured launch wrapper"
```

---

### Task 4: Panel UI — hints and "Configure Launch Wrapper…" menu entry

**Files:**
- Modify: `crates/ai_terminal_panel/Cargo.toml` (add `zed_actions.workspace = true`)
- Modify: `crates/ai_terminal_panel/src/ai_terminal_panel.rs` (`apply_tab_bar_buttons`, ~line 344; `render_launcher`, ~line 1179)

**Interfaces:**
- Consumes: `launcher_hint`, `AiTerminalSettings::try_get` from Task 2; `zed_actions::OpenSettingsAt` (path `"ai_terminal.launcher.command"` — resolves once Task 5 registers that `json_path`; until then the action opens the settings dialog unfocused, which is acceptable mid-plan).

- [ ] **Step 1: Add the zed_actions dependency**

In `crates/ai_terminal_panel/Cargo.toml` `[dependencies]` (alphabetical, after `workspace`):

```toml
zed_actions.workspace = true
```

- [ ] **Step 2: Launcher-button tooltips in `render_launcher`**

At the top of `render_launcher`, after `let agents = self.detected_agents.clone();`:

```rust
        let launcher = AiTerminalSettings::try_get(cx).and_then(|settings| settings.launcher.clone());
```

In the `.children(agents.into_iter().enumerate().map(…))` closure, compute the hint and attach a tooltip:

```rust
            .children(agents.into_iter().enumerate().map(|(ix, agent)| {
                let hint = launcher_hint(&agent, launcher.as_ref());
                let agent_clone = agent.clone();
                Button::new(("agent", ix), agent.name.clone())
                    .start_icon(Icon::new(agent.icon).size(IconSize::Medium))
                    .style(ButtonStyle::Outlined)
                    .size(ButtonSize::Large)
                    .full_width()
                    .when_some(hint, |this, hint| {
                        this.tooltip(Tooltip::text(format!("Launches {hint}")))
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.spawn_agent(&agent_clone, window, cx);
                    }))
            }))
```

- [ ] **Step 3: Menu hints + configure entry in `apply_tab_bar_buttons`**

In `apply_tab_bar_buttons`, precompute hints alongside the agent snapshot. Replace:

```rust
        let agents_for_menu: Arc<Vec<AiAgent>> = Arc::new(self.detected_agents.clone());
```

with:

```rust
        let launcher = AiTerminalSettings::try_get(cx).and_then(|settings| settings.launcher.clone());
        let agents_for_menu: Arc<Vec<(AiAgent, Option<String>)>> = Arc::new(
            self.detected_agents
                .iter()
                .map(|agent| {
                    let hint = launcher_hint(agent, launcher.as_ref());
                    (agent.clone(), hint)
                })
                .collect(),
        );
```

In the `ContextMenu::build` closure, adjust the loop and append the configure entry after it:

```rust
                            for (agent, hint) in agents.iter() {
                                let agent_clone = agent.clone();
                                let weak = weak_panel.clone();
                                let label = match hint {
                                    Some(hint) => format!("{} · {hint}", agent.name),
                                    None => agent.name.clone(),
                                };
                                menu = menu.entry(label, None, move |window, cx| {
                                    if let Some(panel) = weak.upgrade() {
                                        panel.update(cx, |panel, cx| {
                                            panel.spawn_agent(&agent_clone, window, cx);
                                        });
                                    }
                                });
                            }
                            menu = menu.separator().entry(
                                "Configure Launch Wrapper…",
                                None,
                                move |window, cx| {
                                    window.dispatch_action(
                                        Box::new(zed_actions::OpenSettingsAt {
                                            path: "ai_terminal.launcher.command".into(),
                                        }),
                                        cx,
                                    );
                                },
                            );
                            menu
```

The existing `SettingsStore` observer already re-runs `refresh_toolbar_placement` (which rebuilds this snapshot) and `cx.notify()` (which re-renders the launcher), so hint changes apply live — no new subscription needed.

- [ ] **Step 4: Verify**

Run: `cargo check -p ai_terminal_panel -j2 && cargo test -p ai_terminal_panel -j2`
Expected: clean. Optional manual smoke: `cargo run -j2` with `"ai_terminal": { "launcher": { "command": "load run {agent} --" } }` in settings — launcher buttons show "Launches via load" tooltips, + menu entries show "· via load", the configure entry opens the settings dialog.

- [ ] **Step 5: Commit**

```bash
git add crates/ai_terminal_panel/Cargo.toml crates/ai_terminal_panel/src/ai_terminal_panel.rs
git commit -m "feat: show launch-wrapper hints and configure entry in AI panel"
```

---

### Task 5: Settings dialog — "Coding Tools" section

**Files:**
- Modify: `crates/settings_ui/src/page_data.rs` (`panels_page`, ~line 4412)

**Interfaces:**
- Consumes: `SettingsContent.ai_terminal.launcher` (Task 1). Registers `json_path`s `"ai_terminal.launcher.enabled"` / `"ai_terminal.launcher.command"`, which makes Task 4's `OpenSettingsAt { path: "ai_terminal.launcher.command" }` land on the field.

- [ ] **Step 1: Add the section function inside `panels_page`**

Following the existing per-panel section pattern (nested `fn`s inside `panels_page`):

```rust
    fn coding_tools_section() -> [SettingsPageItem; 3] {
        [
            SettingsPageItem::SectionHeader("Coding Tools"),
            SettingsPageItem::SettingItem(SettingItem {
                title: "Wrap Agent Launches",
                description: "Launch AI agent CLIs through the configured launch wrapper command.",
                field: Box::new(SettingField {
                    json_path: Some("ai_terminal.launcher.enabled"),
                    pick: |settings_content| {
                        settings_content
                            .ai_terminal
                            .as_ref()?
                            .launcher
                            .as_ref()?
                            .enabled
                            .as_ref()
                    },
                    write: |settings_content, value| {
                        settings_content
                            .ai_terminal
                            .get_or_insert_default()
                            .launcher
                            .get_or_insert_default()
                            .enabled = value;
                    },
                }),
                metadata: None,
                files: USER,
            }),
            SettingsPageItem::SettingItem(SettingItem {
                title: "Launch Wrapper Command",
                description: "Command template that wraps AI agent launches. {agent} is replaced with the agent id, {command} with the agent executable; a template with neither is used as a prefix. Empty disables wrapping.",
                field: Box::new(SettingField {
                    json_path: Some("ai_terminal.launcher.command"),
                    pick: |settings_content| {
                        settings_content
                            .ai_terminal
                            .as_ref()?
                            .launcher
                            .as_ref()?
                            .command
                            .as_ref()
                            .or(DEFAULT_EMPTY_STRING)
                    },
                    write: |settings_content, value| {
                        settings_content
                            .ai_terminal
                            .get_or_insert_default()
                            .launcher
                            .get_or_insert_default()
                            .command = value.filter(|command| !command.is_empty());
                    },
                }),
                metadata: Some(Box::new(SettingsFieldMetadata {
                    placeholder: Some("load run {agent} --"),
                    ..Default::default()
                })),
                files: USER,
            }),
        ]
    }
```

- [ ] **Step 2: Register the section**

Add `coding_tools_section(),` to the `concat_sections!` invocation at the end of `panels_page`, after the last existing section.

- [ ] **Step 3: Verify**

Run: `cargo check -p settings_ui -j2 && cargo test -p settings_ui -j2`
Expected: clean check; settings_ui's own tests (json-path uniqueness/coverage, if present) pass.

- [ ] **Step 4: Commit**

```bash
git add crates/settings_ui/src/page_data.rs
git commit -m "feat: add Coding Tools launch wrapper section to settings dialog"
```

---

### Task 6: Final verification pass

**Files:** none (verification only; fix-ups as needed)

- [ ] **Step 1: Run the full test suite for touched crates**

Run: `cargo test -p ai_terminal_panel -p settings_ui -p settings_content -p settings -j2`
Expected: all pass.

- [ ] **Step 2: Clippy (release build — takes a while on this machine; plug in first)**

Run: `./script/clippy -p settings_content -p ai_terminal_panel -p settings_ui -- -j2` — if the script rejects trailing flags, run `CARGO_BUILD_JOBS=2 ./script/clippy -p settings_content -p ai_terminal_panel -p settings_ui`
Expected: no warnings (script denies warnings).

- [ ] **Step 3: Manual smoke test (requested from user if a debug build is too slow here)**

With `"ai_terminal": { "launcher": { "command": "load run {agent} --" } }`:
1. Launcher button tooltip shows "Launches via load"; + menu shows "Claude Code · via load".
2. Launching Claude runs `load run claude -- …` (visible in the terminal's process list / loadout's render output) and Claude still gets its `/ide` env (run `/ide` in Claude to confirm).
3. Settings dialog → Panels → Coding Tools shows the toggle and the template field; "Configure Launch Wrapper…" in the + menu jumps there.
4. Toggling "Wrap Agent Launches" off makes the next launch raw (no loadout render output).

- [ ] **Step 4: Commit any fix-ups**

```bash
git add -A && git commit -m "fix: address clippy/test findings for launch wrapper"
```

(Skip if nothing changed.)
