# Zerminal

A terminal-first development environment for agentic coding, forked from [Zed](https://github.com/zed-industries/zed).

![Zerminal's default view — a single terminal and little else](.github/assets/screenshot-terminal.png)

## What is Zerminal?

Zerminal puts the terminal at the center of the IDE experience. The terminal is the primary workspace; the editor is secondary, used to review and steer rather than to write. Projects are auto-detected from the active terminal's working directory — panels, git status, and AI agents all follow along.

It's designed for one specific workflow: using CLI coding agents (Claude Code, Codex, Aider, and the like). You run commands, review context, and steer the agent. The agent writes the code.

Context appears when it's useful — a file browser, a git panel, a memory file — and goes away when it isn't. The goal is minimal chrome around a lot of terminal.

![A terminal with an AI agent pane docked on the right](.github/assets/screenshot-agent.png)
*Agents live in a dedicated pane. Context panels come in from the side when you want them.*

![Three agent terminals running side by side](.github/assets/screenshot-three-agents.png)
*Split the workspace across multiple agents when a single one isn't enough.*

## Status

Zerminal is a personal project, currently at v0.1.x. It's developed and tested on macOS (Apple Silicon) and Fedora Linux. There is no release cadence and no support SLA. It's shared openly in case it's useful to anyone with the same itch; if something breaks for you, an issue is welcome but may take time.

## Install

Pre-built binaries for each release live on the [releases page](https://github.com/elleryfamilia/zerminal/releases/latest).

### macOS (Apple Silicon)

```bash
curl -L -o Zerminal.dmg https://github.com/elleryfamilia/zerminal/releases/latest/download/Zerminal-aarch64.dmg
open Zerminal.dmg
```

Intel Macs are not built. [Build from source](#build-from-source) instead.

### Debian / Ubuntu

Download the latest `zerminal_*_amd64.deb` from the [releases page](https://github.com/elleryfamilia/zerminal/releases/latest), then:

```bash
sudo dpkg -i zerminal_*_amd64.deb
```

The `.deb` is GPG-signed.

### Fedora / RHEL

```bash
sudo rpm --import https://github.com/elleryfamilia/zerminal/releases/latest/download/zerminal-rpm-signing-key.asc
# Download the latest zerminal-*.x86_64.rpm from the releases page, then:
sudo dnf install ./zerminal-*.x86_64.rpm
```

### Arch Linux

```bash
curl -LO https://github.com/elleryfamilia/zerminal/releases/latest/download/PKGBUILD
makepkg -si
```

## Build from source

```bash
cargo run -p zerminal
```

On Linux, install system dependencies first — see [Building for Linux](./docs/src/development/linux.md).

On macOS, Zed's upstream build prerequisites apply; see [Zed's macOS build guide](https://github.com/zed-industries/zed/blob/main/docs/src/development/macos.md).

## How it differs from Zed

- **No built-in AI agent.** Zerminal ships without Zed's agent panel, billing, and account system. You bring your own CLI agent (Claude Code, Codex, Aider, etc.) and run it in a terminal.
- **Terminal-first workspace.** Opens into a terminal rather than an editor welcome screen. The active terminal's working directory drives project detection, not a file you happened to open.
- **Dedicated AI terminal pane.** A persistent right-dock pane for whichever agent CLI you prefer, separate from your command terminals.
- **Less chrome.** No breadcrumbs, no collaboration UI, no extensions marketplace, simplified split behavior, fewer panel toggles.

See [`docs/docs/01-vision.md`](./docs/docs/01-vision.md) for the full rationale.

## Non-goals

- A built-in AI chat or agent. Users bring their own CLI agent.
- A plugin or extension marketplace. Zerminal is a fork, not a platform.
- Collaboration or multiplayer features.

## Relationship to Zed

Zerminal exists because Zed's direction — a batteries-included editor with an integrated agent, collaboration, and an extension ecosystem — points away from the terminal-first tool I wanted to use. That's a reasonable direction for Zed; it just isn't this one. Zerminal keeps Zed's excellent foundation (GPUI, the terminal, the editor core, tree-sitter, LSP) and strips or replaces the rest.

Upstream changes from [`zed-industries/zed`](https://github.com/zed-industries/zed) are cherry-picked selectively, not merged wholesale. The rebrand is deliberate: Zerminal installs as its own application alongside Zed, not as a replacement.

## Contributing

Issues and small pull requests are welcome, but this is a personal project with an opinionated scope. Before opening a substantial PR, please read [`docs/docs/01-vision.md`](./docs/docs/01-vision.md) — changes that move the project toward a general-purpose IDE or replicate non-goals are unlikely to be merged.

## License

Zerminal inherits Zed's licensing:

- Zerminal and Zed editor code: [GPL-3.0-or-later](./LICENSE-GPL)
- GPUI framework: [Apache-2.0](./LICENSE-APACHE)
- Pre-rebrand Zed history: [AGPL-3.0](./LICENSE-AGPL)

See the individual `LICENSE-*` files for full terms. Third-party dependency licenses are managed via `cargo-about`.
