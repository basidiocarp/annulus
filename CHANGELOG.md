# Changelog

All notable changes to Annulus are documented in this file.

## [Unreleased]

## [0.5.1] - 2026-04-14

### Fixed

- **Host-aware provider routing**: `transcript_path` now forces Claude and `session_path` now infers Codex or Gemini before config or recency fallback, so mixed-provider terminals do not render Claude model labels with missing Claude context data.
- **Statusline model coverage**: added built-in context-window recognition and pricing aliases for current GPT-5 and Gemini 2.5 model families so new model names do not fall back to stale defaults as often.

## [0.4.2] - 2026-04-14

### Fixed

- **musl release linking**: bundle SQLite for musl targets so static release builds do not depend on a system `sqlite3` library.

## [0.4.1] - 2026-04-14

### Fixed

- **Windows hook lookup**: `validate-hooks` now resolves PATH entries with `split_paths` and honors `PATHEXT`, so known binaries are found correctly on Windows runners.
- **CI validation**: corrected the formatting and validation surface for the new hook lookup path so the release branch passes the shared CI gate.

## [0.4.0] - 2026-04-14

### Added

- **Codex provider**: reads `~/.codex/sessions/` NDJSON transcripts with delta token resolution and cache alias normalization.
- **Gemini provider**: reads `~/.gemini/tmp/` JSON session files with cumulative/per-turn token semantics.
- **Recency-based auto-detect**: `detect_by_recency()` picks the most recently active provider when stdin doesn't specify one.
- **Provider wiring**: statusline renders token usage from whichever provider is active, not just Claude.
- **Extended pricing**: o3-mini, o4-mini, gpt-4.1, gemini-2.5-pro, gemini-2.5-flash model pricing.
- **PATH-based hook validation**: `validate-hooks` now checks whether hook commands exist on `$PATH` and reports their resolved location.

### Changed

- Removed cortina from default statusline segments (still available for explicit opt-in).

### Fixed

- Removed stale `#[allow(dead_code)]` annotations now that providers are fully wired.

## [0.3.0] - 2026-04-12

### Added

- **Config-driven statusline**: loads segment ordering, visibility, and model-specific context limits from `~/.config/annulus/statusline.toml`.
- **Segment trait and registry**: refactored hardcoded render blocks into a composable trait-based registry with `segments_from_config()`.
- **Context window progress bar**: visual `[ctx ████████░░░░ 67%]` segment with model-aware limits and color thresholds.
- **Tiered pricing**: cache token rates above 200k prompt tokens match Claude's actual billing tiers.

## [0.2.0] - 2026-04-11

### Added

- **Statusline rendering**: full port from cortina with context %, token usage, cost, model name, git branch, workspace name, and mycelium savings segments.
- **Hook path validation**: reads Claude Code settings files, extracts hook command paths, validates existence and executable permissions, reports stale/broken hooks.
- **Graceful degradation**: missing tools produce no output instead of errors; terminal fallback mode works without stdin JSON.

## [0.1.0] - 2026-04-11

### Added

- **Crate scaffold**: initial binary with clap CLI surface.
- **Statusline subcommand**: stub for operator status bar rendering.
- **Validate-hooks subcommand**: stub for hook path validation.
- **CI/coverage/release workflows**: shared ecosystem pipelines.
