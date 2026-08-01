# Agent launch wrapper — design

Date: 2026-07-31
Status: approved for planning

## Objective

Let an external launcher program wrap AI agent launches in the AI terminal
panel. The first use case is loadout (`load run <agent>` refreshes rendered
context before exec'ing the agent), but the mechanism is generic: any wrapper
that prefixes the agent command works (e.g. `distrobox enter dev --`,
`env FOO=bar`, a shell shim). Configured once, visible in the UI, never a gate:
when the wrapper doesn't apply, agents launch exactly as today.

## Context

- `crates/ai_terminal_panel/src/agent_detection.rs` detects agent CLIs from
  `coding_tools::KNOWN_TOOLS` plus `ai_terminal.agents` settings overrides,
  producing `AiAgent { id, name, args, env, icon, path }`.
- `AiTerminalPanel::spawn_agent` builds a `SpawnInTerminal` with
  `command: agent.path`, `args: agent.args`, injects `/ide` attachment env for
  `claude`/`copilot`, and spawns a terminal task.
- The settings dialog (`crates/settings_ui`) renders typed fields (toggle,
  text input, dropdown) per page/section; simple scalar settings get stock
  editors, arrays/maps do not.
- Loadout's CLI: `load run <agent> [-- args…]` pulls config, renders overlays,
  then execs the agent. `load agents --json` lists its agents, but this design
  deliberately does not query loadout — the wrapper is tool-agnostic.

## Settings shape

New optional object under the existing `ai_terminal` namespace:

```jsonc
"ai_terminal": {
  "launcher": {
    // Master switch. Defaults to true; lets the dialog toggle wrapping
    // off without erasing the template.
    "enabled": true,
    // Wrapper command template as a single string, shell-split at spawn
    // time (quotes respected). Placeholders:
    //   {agent}   → zerminal's agent id ("claude", "codex", …)
    //   {command} → the resolved agent executable path
    "command": "load run {agent} --",
    // Optional allowlist of agent ids; omitted = wrap every agent.
    // JSON-only in v1 (no dialog UI).
    "agents": ["claude", "codex", "gemini", "copilot", "cursor-agent"]
  }
}
```

- No `launcher` object, empty/missing `command`, or `enabled: false` →
  behavior identical to today.
- The agent's own configured `args` are appended after the substituted
  template (`load run claude -- --model opus` composes naturally; the
  trailing `--` lives in the template where the wrapper needs it).
- If the template contains `{command}`, the agent binary is launched by the
  wrapper explicitly; if it contains only `{agent}`, the wrapper is expected
  to resolve the agent itself (loadout's model). Both placeholders are
  optional; a template with neither is a pure prefix and the resolved agent
  path is appended before the agent args.

## Spawn flow

A pure function in `agent_detection.rs`:

```rust
fn apply_launcher(agent: &AiAgent, launcher: &LauncherConfig) -> (PathBuf, Vec<String>)
```

- Returns the unmodified `(agent.path, agent.args)` when the launcher is
  disabled, the template is empty/invalid, or the agent id is filtered out by
  the allowlist.
- Otherwise shell-splits the template, substitutes placeholders, resolves
  argv[0] with the same `find_in_path` lookup agents use (falling back to the
  literal so a missing wrapper surfaces a clear "not found" in the terminal
  tab), and appends the agent args.

`spawn_agent` calls it when building `SpawnInTerminal`. Everything else —
agent identity (`agent.id`), `/ide` attachment prep, env injection, cwd,
attention notifications, quit/close guards — is untouched; identity and env
never depended on the command line.

## Error handling

- Malformed template (unbalanced quotes, empty after split): `log::warn!`,
  launch raw. The wrapper is an enhancement, never a gate.
- Configured-but-missing wrapper binary: **not** silently bypassed — the user
  asked for wrapped launches, so the spawn error shows in the tab (same
  behavior as a bad agent `command` override today).
- Agent outside the allowlist: silently raw, by design.

## UI

1. **Settings dialog — "Coding Tools" section** with two stock
   `SettingField` items:
   - *Wrap agent launches* — toggle bound to `ai_terminal.launcher.enabled`.
   - *Launch wrapper command* — text field bound to
     `ai_terminal.launcher.command`, with placeholder help text documenting
     `{agent}` / `{command}`.
2. **AI panel + menu** gains one entry, "Configure Launch Wrapper…", which
   opens the settings dialog at that section.
3. **Wrapped-launch indicator**: launcher buttons and + menu entries for
   agents that will be wrapped show a subdued hint ("via load" — derived from
   the template's argv[0]) as a suffix/tooltip.
4. Settings changes are picked up live by the panel's existing
   `SettingsStore` observer — no restart.

No per-launch picker; the wrapper is a global configuration.

## Testing

Unit tests in `agent_detection.rs` alongside the existing ones:

- placeholder substitution for `{agent}` and `{command}`;
- agent args appended after the template;
- pure-prefix template (no placeholders) appends the agent path then args;
- allowlist filtering (listed wrapped, unlisted raw, absent list = all);
- `enabled: false`, missing template, and malformed template fall back raw;
- quoted template segments survive shell-splitting.

No E2E: downstream spawn plumbing is unchanged.

## Risks & notes

- Settings schema addition touches `settings_content` (same pattern as the
  existing `ai_terminal.agents`).
- Loadout expects *its* agent ids; zerminal's `cursor-agent` vs loadout's
  `cursor` differ. The allowlist keeps mismatches out; a per-agent id remap is
  deliberate YAGNI for v1.
- `load run` pulls config and renders before exec, adding a little launch
  latency; inherent to the use case. A future knob could pass `--skip-render`.

## Rollback

Purely additive: removing the `launcher` settings object (or toggling
`enabled` off) restores today's behavior; reverting the feature commits
removes the schema addition cleanly.

## First implementation step

Add `LauncherConfig` to `AiTerminalSettings` in `agent_detection.rs` (settings
content + resolution), with `apply_launcher` and its unit tests.
