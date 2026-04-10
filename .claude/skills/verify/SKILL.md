---
name: verify
description: Run clippy lint and cargo build to verify the project compiles cleanly. Use after making changes to check for errors.
---

Run the following checks in sequence. Stop and report on first failure:

1. `cargo clippy --workspace --all-targets 2>&1` — Check for lint warnings and errors
2. `cargo build --workspace 2>&1` — Verify the project compiles

If the `sensevoice` feature was modified, also run:
3. `cargo build --workspace --features sensevoice 2>&1`

Report a summary of any warnings or errors found.
