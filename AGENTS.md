# Annulus Agent Notes

## Purpose

Annulus provides cross-ecosystem operator utilities — small, read-only tools that span multiple data sources. Work here produces statusline rendering, hook path validation, and (future) training data export. Keep the surface narrow: if a utility only touches one tool's data, it belongs in that tool, not here.

---

## Source of Truth

- `src/` — all utility logic; this is the authoritative implementation.
- `../septa/` — authoritative cross-tool payload shapes; update contract schemas there before changing output formats.
- `../ecosystem-versions.toml` — shared dependency pins; check before upgrading `spore` or other shared crates.
- `../docs/architecture/annulus-design-note.md` — design rationale, confirmed utilities, and the bar for adding new ones.

---

## Before You Start

Before writing code, verify:

1. **Design note**: read `../docs/architecture/annulus-design-note.md` — confirm the utility belongs in annulus, not in the owning tool.
2. **Versions**: read `../ecosystem-versions.toml` — verify shared dependency pins before upgrading.
3. **Contracts**: if the change affects output format consumed by other tools (e.g. `stipe doctor`), update `../septa/` schemas first.

---

## Preferred Commands

Use these for most work:

```bash
cargo build --release
cargo test
```

For targeted work:

```bash
cargo clippy
cargo fmt --check
```

---

## Repo Architecture

Annulus is a single crate, single binary. Not a workspace.

Key boundaries:

- `src/statusline.rs` — owns segment rendering. Each data source is independent.
- `src/validate_hooks.rs` — owns hook path checking. Output is suitable for both direct use and piped into `stipe doctor`.
- `src/main.rs` — clap CLI routing only. No business logic here.

Current direction:

- Statusline will gain segment implementations as spore discovery is wired up.
- Hook validation will gain config file parsing and path checking logic.
- Keep utilities read-only. Annulus does not write to ecosystem tools.

---

## Working Rules

- Annulus is read-only. Never write to ecosystem tool databases or config files.
- Use spore discovery to detect which tools are available. Degrade gracefully when tools are missing.
- Keep the surface narrow. The bar for adding a new utility: small, operator-facing, read-only or diagnostic, doesn't fit any existing tool, and genuinely cross-cutting.
- Run `cargo clippy` and `cargo fmt --check` before closing any implementation task.

---

## Done Means

A task is not complete until:

- [ ] `cargo test` passes
- [ ] `cargo clippy` passes with no warnings
- [ ] `cargo fmt --check` passes
- [ ] Any skipped validation or follow-up work is stated explicitly
