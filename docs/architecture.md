# Annulus Architecture

Annulus is a single-crate Rust binary providing cross-ecosystem operator
utilities. Its core job is to host small, read-only tools that span multiple
data sources and don't belong in any single existing tool. This document covers
the system boundary, utility model, and design principles.

---

## Design Principles

- **Read-only** -- annulus reads data from ecosystem tools but does not write to
  them or maintain its own persistent state.
- **Discovery-driven** -- each utility detects which tools are available via
  spore discovery. Missing tools produce no output, not errors.
- **Segment-based** -- the statusline is composed of independent segments, each
  responsible for one data source. Segments are composable and configurable.
- **Narrow surface** -- the bar for adding a new utility is high: small,
  operator-facing, read-only or diagnostic, doesn't fit any existing tool, and
  genuinely cross-cutting (multiple tools are data sources or consumers).
- **Graceful degradation** -- a user with only mycelium installed sees token
  savings. A user with the full ecosystem sees agents, tasks, memory health,
  and backend status. The statusline adapts to what's available.

---

## System Boundary

### Annulus owns

- Operator-facing statusline rendering across ecosystem data sources
- Hook path validation across host config files
- (Future) Training data export from hyphae session records

### Annulus does NOT own

- Signal capture or lifecycle events (cortina)
- Memory storage or retrieval (hyphae)
- Agent session management (volva)
- Hook authoring or skill packaging (lamella)
- Installation or ecosystem repair (stipe)
- Task coordination or evidence (canopy)

### The New-Utility Bar

Before adding a utility to annulus, it must pass all three questions:

1. Is the responsibility stable and first-class?
2. Does it fit an existing tool? (If yes, put it there.)
3. Do more than one repos benefit from it living here?

---

## Workspace Structure

```text
src/
├── main.rs            CLI entry point and subcommand dispatch
├── statusline.rs      segment rendering and composition
└── validate_hooks.rs  hook path validation
```

- **main.rs**: Clap CLI routing. No business logic -- delegates to utility
  modules immediately.
- **statusline.rs**: Will grow segment implementations as spore discovery is
  wired up. Each segment is an independent function that returns an optional
  rendered string.
- **validate_hooks.rs**: Will gain config file parsing (settings.json and other
  host configs) and path existence/executable checks.

---

## Statusline Segment Model

Each data source is an independent segment:

| Segment | Source | Shows |
|---------|--------|-------|
| context | transcript (stdin) | context %, tokens, cost |
| git | git CLI | branch, dirty state |
| mycelium | SQLite history.db | tokens saved |
| hyphae | SQLite | memory health |
| canopy | state files | active agents, task count |
| volva | status probe | backend status |
| model | transcript or stdin | compact model name |

Segments render only if the data source is available. Ordering, format, and
color themes are configurable via `~/.config/annulus/config.toml`.

---

## Key Dependencies

- **clap** -- CLI contract for subcommand dispatch.
- **anyhow** -- application-layer error handling.
- **spore** (future) -- tool discovery and shared path resolution.
