# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

Annulus is a small Rust binary providing cross-ecosystem operator utilities for the Basidiocarp ecosystem. It owns lightweight, read-only tools that span multiple data sources and don't belong in any single existing tool. Current utilities are statusline rendering and hook path validation.

---

## What Annulus Does NOT Do

- Does not capture lifecycle signals or session events (Cortina owns that)
- Does not store memory or indexed documents (Hyphae owns that)
- Does not manage agent sessions or execution hosting (Volva owns that)
- Does not package hooks, skills, or prompts (Lamella owns that)
- Does not handle installation or ecosystem repair (Stipe owns that)
- Does not track tasks or coordination records (Canopy owns that)

---

## Failure Modes

- **Tool not installed**: segment renders nothing instead of erroring. Missing tools produce no output.
- **Data source unavailable**: statusline degrades gracefully — only available segments appear.
- **Invalid hook path**: reported as stale or broken, not silently skipped.
- **Config file missing**: uses defaults; does not create files on behalf of the user.

---

## State Locations

| What | Path |
|------|------|
| Config file | `~/.config/annulus/config.toml` (future) |
| Log output | stderr |

Annulus is read-only by design. It does not maintain its own database or persistent state.

---

## Build & Test Commands

```bash
cargo build --release
cargo install --path .

cargo test
cargo clippy
cargo fmt --check
cargo fmt
```

---

## Architecture

```text
src/
├── main.rs            # CLI entry point and subcommand dispatch
├── statusline.rs      # Segment rendering and composition
└── validate_hooks.rs  # Hook path validation
```

- **main.rs**: Clap CLI with `statusline` and `validate-hooks` subcommands.
- **statusline.rs**: Reads from ecosystem tools (git, mycelium, hyphae, canopy, volva) via CLI probes or direct file access. Each data source is an independent segment.
- **validate_hooks.rs**: Reads host config files and checks that registered hook paths exist and are executable.

---

## Key Design Decisions

- **Single binary, not a workspace** — these utilities are small and don't need separate release cadence.
- **Read-only** — annulus reads data from ecosystem tools but does not write to them or maintain its own state.
- **Discovery-driven** — segments only render if the data source is available. No errors for missing tools.
- **Segment-based statusline** — each data source is an independent segment with its own rendering logic, composable and configurable.

---

## Key Files

| File | Purpose |
|------|---------|
| `src/main.rs` | CLI entry point and subcommand routing |
| `src/statusline.rs` | Statusline segment rendering |
| `src/validate_hooks.rs` | Hook path validation logic |

---

## Communication Contracts

### Outbound (this project sends)

Annulus does not emit structured payloads to sibling tools in the current scaffold.

### Inbound (this project reads)

| Source | Protocol | What |
|--------|----------|------|
| git CLI | Shell exec | Branch name, dirty state |
| mycelium SQLite | File read | Token savings |
| hyphae SQLite | File read | Memory health |
| canopy state | CLI or file read | Active agents, task counts |
| volva status | CLI probe | Backend status |
| Host config files | File read | Hook paths for validation |

### Shared Dependencies

- **spore** (future): tool discovery and shared path resolution. Not yet a dependency — added when segment implementations land.

---

## Testing Strategy

- Unit tests cover segment rendering with mock data sources and hook validation with fixture config files.
- Integration tests verify CLI output and exit codes.
- Treat statusline segment additions as requiring both unit and manual visual verification.
