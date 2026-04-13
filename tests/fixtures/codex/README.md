# Codex fixture provenance

## Capture method

`sample-session.jsonl` was constructed by hand on 2026-04-13 to match the NDJSON
schema documented in `annulus/docs/providers/codex.md`.

The schema was sourced from two authoritative references in the basidiocarp
audit corpus:

- `/.audit/external/sources/ccusage/apps/codex/src/data-loader.ts` — the ccusage
  Codex CLI parser. Contains in-source vitest fixtures showing exactly which
  fields are emitted and how cumulative vs. delta accounting works.
- `/.audit/external/sources/ccusage/apps/codex/CLAUDE.md` — field-level
  documentation for the Codex JSONL schema written by the ccusage maintainers.
- `/cap/server/lib/usage/parse-codex.ts` — the cap dashboard's own Codex
  session parser, showing which entry types carry token counts.
- `/.audit/external/sources/CodexBar/docs/codex.md` — CodexBar's documentation
  of the cost-usage scanner, confirming paths and the `event_msg`/`token_count`
  structure.

The Codex CLI was not run locally; no real session data was captured. The
fixture is a minimal but structurally faithful reconstruction.

## Redaction

- `payload.cwd` replaced with `/home/user/myproject` (generic path).
- `session_meta.payload.id` replaced with a random UUID pattern.
- Any user-visible prompt text replaced with `[REDACTED]`.
- Token counts are plausible but not from a real session.

## Gold entries for tests

The fixture contains two assistant turns. Parsers exercising this fixture should
produce:

| Turn | `input_tokens` | `cached_input_tokens` | `output_tokens` | source |
|------|---------------|-----------------------|-----------------|--------|
| 1    | 1200          | 200                   | 500             | `last_token_usage` (direct delta) |
| 2    | 800           | 100                   | 300             | `last_token_usage` (direct delta) |

Cumulative totals after turn 2: `input=2000, cached=300, output=800`.

The per-session aggregate expected from a reader that sums all delta entries:
`prompt_tokens=2000, completion_tokens=800, cache_read_tokens=300, cache_creation_tokens=0`.

Entry types present in the fixture:

- `session_meta` — session context (one entry, first line)
- `turn_context` — model marker (two entries, one per turn)
- `event_msg` with `type: "token_count"` — token accounting (two entries)
- `event_msg` with `type: "user_message"` — user turn (one entry, no token data)
- `response_item` with `type: "message", role: "assistant"` — assistant response marker (two entries, no token data)
