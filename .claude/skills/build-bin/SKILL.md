---
name: build-bin
description: Build and optionally run a specific binary from the workspace. Usage - /build-bin <binary-name> [--run]
---

Build a specific binary from the picc workspace.

Available binaries: `picc`, `axcli`, `ax_tui`, `ax_print`, `standup`, `hidden`, `claude_menubar`, `english-refiner`, `voice-correct`, `clipcopy`

Requires `sensevoice` feature: `dictation`

Usage: `$ARGUMENTS` should be the binary name, optionally followed by `--run` to also execute it.

Steps:
1. Parse `$ARGUMENTS` to get the binary name and whether to run
2. If the binary is `dictation`, add `--features sensevoice`
3. Run `cargo build --bin <name>` (with features if needed)
4. If `--run` was specified and build succeeded, run `cargo run --bin <name>`
5. Report build result
