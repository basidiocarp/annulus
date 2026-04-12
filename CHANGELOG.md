# Changelog

All notable changes to Annulus are documented in this file.

## [Unreleased]

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
