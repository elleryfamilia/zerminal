# Brosh 2 — Vision

## One-liner

A terminal-first development environment for agentic coding, built as a Zed fork.

## Philosophy

- **Terminal-first, not editor-first.** The terminal is the primary workspace. Code editing is secondary — you review and steer, the AI writes.
- **Agentic coding workflow.** The human role is: run commands, review context, review files, steer AI agents. Not: write code, navigate symbols, refactor.
- **Minimal chrome, maximum terminal.** UI elements exist to support the terminal workflow, not to replicate a traditional IDE.
- **Two terminals, always.** A command terminal and an AI agent terminal (Claude Code, Codex, etc.) are the core experience.
- **Context on demand.** File browser, git status, memory/context files appear when needed, dismiss when not.

## Target Users

- Developers using AI coding agents (Claude Code, Codex CLI, Aider, etc.)
- People who live in the terminal and want contextual IDE features without the IDE weight

## Platforms

- macOS (MacBook Air M4)
- Linux (Fedora, Intel N150 — must be lightweight)

## Non-goals

- Built-in AI chat/agent (users bring their own CLI agent)
- Plugin/extension marketplace
- Collaboration/multiplayer
- Remote development (initially)

## What We Keep from Zed

- GPUI rendering engine (Metal/Vulkan, cross-platform)
- Terminal implementation (alacritty_terminal core)
- Full editor with Tree-sitter syntax highlighting and LSP
- Project panel (file browser) — simplified
- Git panel — simplified
- Workspace serialization/persistence
- Keybinding system
- Theme system

## What We Remove from Zed

- Agent panel / built-in AI (agent_ui crate)
- AI subscription/billing/account system
- Collaboration/multiplayer features (collab, call, channel crates)
- Extensions/WASM plugin system (we fork, not extend)
- Breadcrumbs (visual noise for terminal-first workflow)
- Auto-update tied to Zed's infrastructure

## What We Add

- Terminal-first startup (opens into terminal, not editor welcome)
- Dedicated AI agent pane (right dock, persistent, for Claude Code / Codex)
- Context/memory panel (CLAUDE.md, project context files)
- Simplified split behavior (right and down only)
- Larger, more prominent panel toggles
- Terminal-optimized keybindings
- Streamlined UI chrome (fewer buttons, less visual noise)
