# Risks and Open Decisions

## Risks

### 1. Upstream Drift
Zed is actively developed. The further the fork diverges, the harder it is to pull upstream improvements (performance fixes, terminal improvements, GPUI enhancements).

**Mitigation:**
- Keep changes in new crates where possible (ai_terminal_panel, context_panel)
- Minimize modifications to existing crates — prefer deletion over modification
- Rebase periodically on upstream, especially for gpui/terminal/editor crates
- Accept that eventually this becomes a hard fork, not a tracking fork

### 2. Build Complexity
Zed has significant platform-specific build requirements (Metal shaders on macOS, Vulkan setup on Linux, C dependencies).

**Mitigation:**
- Document build steps thoroughly in Phase 0
- Test on both target machines (MacBook Air M4, MiniBook X) early
- The N150's integrated GPU should handle Vulkan (Intel Alder Lake supports it)

### 3. GPUI Learning Curve
GPUI is Zed-specific. No external documentation, tutorials, or community beyond Zed's codebase.

**Mitigation:**
- Study existing panels (project_panel, git_ui) as examples
- The two new crates (ai_terminal_panel, context_panel) are structurally simple
- ai_terminal_panel is basically terminal_panel with a different default config
- context_panel is a tree view + file opener — well-trodden GPUI patterns

### 4. Performance on N150
The Intel N150 is a low-power chip with integrated graphics. Zed uses GPU acceleration heavily.

**Mitigation:**
- Test early in Phase 0
- Zed is lighter than Electron, so should still be an improvement
- Monitor GPU usage and frame rates
- Stripping crates reduces baseline memory usage

---

## Open Decisions

### 1. Tracking Fork vs Hard Fork?
Do we want to periodically rebase on upstream Zed, or diverge completely?

**Recommendation:** Start as a tracking fork (Phases 0-3). Evaluate after Phase 5 whether upstream changes are still valuable enough to justify rebasing. Most value from upstream will be in gpui, terminal, and editor crates.

### 2. Branding — "Brosh" or New Name?
The v1 is "brosh" on npm/GitHub. The v2 is architecturally different.

**Decision needed:** Keep "Brosh 2" or rename? Affects repo name, binary name, config paths.

### 3. Distribution
How to distribute the binary?

**Options:**
- GitHub Releases (manual download)
- Homebrew tap (macOS)
- COPR or custom RPM repo (Fedora)
- Build from source only (initially)

**Recommendation:** GitHub Releases + build from source. Add Homebrew/COPR later.

### 4. License
Zed is licensed under a combination of GPL, AGPL, and Apache 2.0 depending on the crate. The fork must comply.

**Action:** Audit license requirements before distributing. GPUI is Apache 2.0. Zed app code is GPL/AGPL.
