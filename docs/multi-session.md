# Multi-Session Host Adapter Contract

Annulus accepts JSON on stdin to identify which AI provider session to display. This document specifies the full stdin schema, the provider resolution precedence chain, and integration patterns for each supported host.

---

## Stdin JSON Schema

When a host invokes `annulus statusline`, it may pipe a JSON object to stdin. All fields are optional.

```json
{
  "provider": "codex",
  "session_path": "/path/to/current/session.jsonl",
  "transcript_path": "/path/to/claude/transcript.jsonl",
  "model": { "display_name": "gpt-4.1" },
  "workspace": { "current_dir": "/home/user/project" }
}
```

### Field Reference

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `provider` | string | no | Explicit provider name: `"claude"`, `"codex"`, or `"gemini"`. When present, overrides both config file and auto-detection. When absent, falls back to config or auto-detect. |
| `session_path` | string | no | Absolute path to the active session file for Codex or Gemini. When present, the provider reads this file instead of scanning for the globally most recent session. Ignored for Claude (use `transcript_path` instead). |
| `transcript_path` | string | no | Absolute path to the Claude transcript JSONL file. Claude-specific; provided natively by Claude Code's statusline hook protocol. |
| `model` | object | no | Contains a `display_name` string for the model. Used for pricing lookup and statusline display. |
| `workspace` | object | no | Contains a `current_dir` string for git branch detection and workspace name display. |

---

## Provider Resolution Precedence

Annulus resolves which provider to use through a three-level priority chain. The first level that produces a value wins.

1. **Stdin `provider` field** -- If the host pipes `{"provider": "codex", ...}`, annulus uses Codex regardless of config or session recency.

2. **Config file `provider` setting** -- If `~/.config/annulus/statusline.toml` contains `provider = "codex"`, that provider is used when stdin does not specify one.

3. **Auto-detect by recency** -- When neither stdin nor config specifies a provider, annulus checks which provider (Claude, Codex, Gemini) has the most recently modified session file and uses that one. Ties favor Claude.

---

## Host Integration Patterns

### Claude Code

No adapter needed. Claude Code natively passes `transcript_path`, `model`, and `workspace` via stdin. Multi-session works automatically because each Claude Code instance passes its own transcript path.

### Codex

Example hook script for Codex sessions:

```bash
#!/usr/bin/env bash
# Statusline hook for Codex -> annulus
# Place in your Codex hooks directory or invoke from your shell prompt.

CODEX_HOME="${CODEX_HOME:-$HOME/.codex}"
SESSION_DIR="$CODEX_HOME/sessions"

# Find the most recently modified session file in this terminal's context.
# If Codex exposes $CODEX_SESSION_FILE in the future, prefer that.
SESSION_FILE=$(find "$SESSION_DIR" -name '*.jsonl' -type f -printf '%T@ %p\n' 2>/dev/null \
  | sort -rn | head -1 | cut -d' ' -f2-)

if [ -z "$SESSION_FILE" ]; then
  annulus statusline
  exit 0
fi

printf '{"provider":"codex","session_path":"%s"}' "$SESSION_FILE" | annulus statusline
```

**macOS note:** The `find -printf` flag is a GNU extension not available on macOS. Use `gfind` from Homebrew coreutils, or replace the find pipeline with:

```bash
SESSION_FILE=$(stat -f '%m %N' "$SESSION_DIR"/*.jsonl 2>/dev/null \
  | sort -rn | head -1 | cut -d' ' -f2-)
```

### Gemini CLI

Example hook script for Gemini sessions:

```bash
#!/usr/bin/env bash
# Statusline hook for Gemini CLI -> annulus

GEMINI_DIR="${GEMINI_HISTORY_DIR:-$HOME/.gemini/tmp}"

# Find the most recently modified session file.
SESSION_FILE=$(find "$GEMINI_DIR" -name '*.json' -type f -printf '%T@ %p\n' 2>/dev/null \
  | sort -rn | head -1 | cut -d' ' -f2-)

if [ -z "$SESSION_FILE" ]; then
  annulus statusline
  exit 0
fi

printf '{"provider":"gemini","session_path":"%s"}' "$SESSION_FILE" | annulus statusline
```

The same macOS caveat applies. Use `gfind` or `stat -f '%m %N'` on macOS.

---

## Multi-Session Scenarios

### Claude terminal A + Codex terminal B

Each host pipes its own `provider` and session path. Each terminal shows its own provider's data independently.

### Two Claude terminals

Both pass their own `transcript_path` natively. Already works without any adapter changes.

### Two Codex terminals

Each hook invocation finds its own most-recent session file and passes it via `session_path`. Works as long as the two sessions have different file paths, which they do -- Codex creates a new JSONL file per session.

### No stdin identity

Falls back to auto-detect by recency (existing behavior). In this case, all terminals show the same globally-most-recent provider's data.

---

## Limitations

- If a host CLI does not expose its current session file path via environment variable, the hook must infer it from the most recently modified file at invocation time. This is best-effort.
- If two sessions from the same provider start within the same second in the same directory, the hook may pick the wrong file. This is unlikely in practice.
- `session_path` is ignored for Claude. Use `transcript_path` for backward compatibility with Claude Code's native protocol.
