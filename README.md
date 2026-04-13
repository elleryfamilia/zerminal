# Zerminal

A terminal-first development environment for agentic coding, forked from [Zed](https://github.com/zed-industries/zed).

Zerminal puts the terminal at the center of the IDE experience. Projects are auto-detected from the terminal's working directory. AI agent terminals, context panels, and git integration all follow the active terminal's CWD.

## Building

```bash
cargo run -p zed
```

See [Building Zed for Linux](./docs/src/development/linux.md) for dependencies.

## Licensing

Zerminal is a fork of [Zed](https://github.com/zed-industries/zed), which is licensed under the following:

- Zed editor: [GPL-3.0-or-later](./LICENSE-GPL)
- GPUI framework: [Apache-2.0](./LICENSE-APACHE)
- Original Zed codebase: [AGPL-3.0](./LICENSE-AGPL) (prior to license change)

See the individual `LICENSE` files for full terms. Third-party dependency licenses are managed via `cargo-about`.
