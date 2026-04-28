# Annulus

Cross-ecosystem operator utilities. Lightweight, read-only tools that span multiple data sources and don't belong in any single existing tool.

Named after the annulus — the small structural ring on a mushroom stipe that connects pieces without being a major organ.

Part of the [Basidiocarp ecosystem](https://github.com/basidiocarp).

---

## The Problem

Operator-facing utilities like statusline rendering and hook path validation are cross-cutting by nature. Statusline reads from git, mycelium, hyphae, canopy, and volva. Hook validation checks config files that reference hooks from lamella, stipe, and user scripts. Neither belongs in a single tool's repo.

## The Solution

Annulus provides a small set of operator-facing utilities that read from ecosystem tools directly, without absorbing their storage or becoming the system of record for anything.

---

## The Ecosystem

| Tool | Purpose |
|------|---------|
| **[annulus](https://github.com/basidiocarp/annulus)** | Cross-ecosystem operator utilities (this project) |
| **[canopy](https://github.com/basidiocarp/canopy)** | Multi-agent coordination ledger |
| **[cap](https://github.com/basidiocarp/cap)** | Web dashboard for the ecosystem |
| **[cortina](https://github.com/basidiocarp/cortina)** | Lifecycle signal capture and session attribution |
| **[hyphae](https://github.com/basidiocarp/hyphae)** | Persistent agent memory |
| **[hymenium](https://github.com/basidiocarp/hymenium)** | Workflow orchestration engine |
| **[lamella](https://github.com/basidiocarp/lamella)** | Skills, hooks, and plugins for coding agents |
| **[mycelium](https://github.com/basidiocarp/mycelium)** | Token-optimized command output |
| **[rhizome](https://github.com/basidiocarp/rhizome)** | Code intelligence via tree-sitter and LSP |
| **[spore](https://github.com/basidiocarp/spore)** | Shared transport and editor primitives |
| **[stipe](https://github.com/basidiocarp/stipe)** | Ecosystem installer and manager |
| **[volva](https://github.com/basidiocarp/volva)** | Execution-host runtime layer |

> **Boundary:** `annulus` owns operator-facing display and validation utilities that are cross-cutting. It reads data from ecosystem tools but does not capture signals (cortina), store memory (hyphae), manage sessions (volva), or package content (lamella).

---

## Quick Start

```bash
cargo install --path .

# Render operator statusline
annulus statusline

# Validate hook paths
annulus validate-hooks
```

---

## Confirmed Utilities

### Statusline

Renders a two-line operator status bar showing context usage, token counts, session cost, model name, git branch, mycelium savings, and (when available) hyphae health and canopy task state. Segment-based and discovery-driven — each data source is an independent segment that only renders if the tool is available.

### Notify

Polls Canopy for pending operator notifications and prints them. `annulus notify --poll` performs notification acknowledgement — it marks seen rows in the Canopy-owned notifications table so they are not repeated on the next poll. This is the only write surface in annulus; all other subcommands are read-only.

```bash
# Check for pending notifications (one-shot)
annulus notify

# Poll and acknowledge new notifications
annulus notify --poll
```

### Hook Path Validator

Reads host config files, checks that every registered hook path exists and points to a valid executable, and reports stale, missing, or broken entries. Output is suitable for both direct use and piped into `stipe doctor`.

### Bridge File

Hooks and external tools can write state to a well-known JSON file that annulus reads and renders in the statusline. Entries support optional TTL (time-to-live), so transient state like focus modes or badges can auto-expire without explicit cleanup.

**File Location**: `~/.config/annulus/bridge.json`

**Schema**:
```json
{
  "entries": [
    {
      "key": "mode",
      "value": "focus",
      "ttl_secs": 300,
      "written_at": 1704067200
    }
  ]
}
```

- `key`: identifier for this state entry (e.g., "mode", "badge")
- `value`: the state value to render
- `ttl_secs` (optional): how many seconds this entry should live. Combined with `written_at`, entries older than `written_at + ttl_secs` are automatically filtered.
- `written_at` (optional): Unix timestamp (seconds) when the entry was written. If absent, the entry never expires. If `ttl_secs` is also absent, staleness check is skipped.

**Enabling the Bridge Segment**: Add `bridge` to your `~/.config/annulus/statusline.toml`:
```toml
[[segments]]
name = "bridge"
enabled = true
```

**Example Hook Usage**:
```bash
#!/usr/bin/env bash
# A hook that sets a focus mode, auto-expiring after 5 minutes
BRIDGE_FILE="$HOME/.config/annulus/bridge.json"
mkdir -p "$(dirname "$BRIDGE_FILE")"

NOW=$(date +%s)
cat > "$BRIDGE_FILE" <<EOF
{
  "entries": [
    {
      "key": "mode",
      "value": "focus",
      "ttl_secs": 300,
      "written_at": $NOW
    }
  ]
}
EOF
```

---

## Multi-Session Support

Annulus supports multiple simultaneous AI provider sessions. When a host (Claude Code, Codex, Gemini CLI) pipes JSON to stdin, host-specific fields such as `transcript_path` and `session_path` identify which session to display; an explicit `provider` can still override that when needed. This lets separate terminals show independent statuslines for different providers or different sessions of the same provider.

Provider resolution follows a three-level precedence chain: explicit stdin `provider` field, then config file setting, then auto-detection by most recent session file.

See [docs/multi-session.md](docs/multi-session.md) for the full stdin schema, precedence rules, and host integration examples.

---

## Architecture

```text
annulus (single binary)
├── src/main.rs            CLI entry, subcommand dispatch
├── src/statusline.rs      segment rendering and composition
└── src/validate_hooks.rs  hook path validation
```

```text
annulus statusline       render operator status bar
annulus validate-hooks   check hook path health
```

---

## Development

```bash
cargo build --release
cargo test
cargo clippy
cargo fmt
```

## License

MIT — see [LICENSE](LICENSE).
