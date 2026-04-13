# Gemini CLI Session Fixture

## Provenance

- **Source**: Constructed from the google-gemini/gemini-cli source schema
  (v0.1.x). No real user session was captured; the structure mirrors the
  checkpoint file format observed in the open-source repository.
- **Handoff**: #119a (annulus/gemini-format-spec), 2026-04-13.
- **CLI version**: google-gemini/gemini-cli v0.1.x (format stable since initial
  public release).
- **Platform observed**: macOS (`~/.gemini/tmp/<uuid>.json`).

## Redaction

All `text` fields under `parts` are replaced with `[REDACTED: ...]` strings.
No actual prompts or model replies are retained. Token counts (`usageMetadata`)
are representative values derived from typical session sizes; they are not from
a real session.

## File

`sample-session.json` — a JSON array representing a four-user-turn session with
four model turns. One model turn (turn 3) intentionally omits `usageMetadata`
to exercise the reader's tolerance for missing usage fields (observed in
tool-use turns in the real CLI).

## Gold entries for tests

The reader implementation (#119b) should exercise the following:

| Turn | Role | usageMetadata present | promptTokenCount | candidatesTokenCount |
|------|------|-----------------------|-----------------|---------------------|
| 1 | model | yes | 312 | 87 |
| 2 | model | yes | 847 | 203 |
| 3 | model | no | — (skip) | — (skip) |
| 4 | model | yes | 1534 | 411 |

Expected aggregation using the recommended reader strategy:
- `prompt_tokens`: `1534` (last model turn's `promptTokenCount`)
- `completion_tokens`: `87 + 203 + 411 = 701` (sum of all `candidatesTokenCount`)
- `cache_read_tokens`: `0` (not reported by Gemini CLI)
- `cache_creation_tokens`: `0` (not reported by Gemini CLI)
