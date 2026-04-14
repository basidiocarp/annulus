//! Codex CLI token usage provider.
//!
//! Reads token usage from Codex CLI NDJSON session files. Sessions are stored
//! under `$CODEX_HOME/sessions/YYYY/MM/DD/<id>.jsonl` (date hierarchy) and
//! older ones archived to `$CODEX_HOME/archived_sessions/<id>.jsonl` (flat).
//!
//! Path resolution order:
//! 1. `$CODEX_HOME` environment variable, if set.
//! 2. `~/.codex` as the default base directory.
//!
//! `is_available()` returns `true` when the Codex base directory exists.
//! `session_usage()` finds the most recent JSONL session file and accumulates
//! token usage from `event_msg` entries carrying `token_count` payloads.
//! Malformed lines are skipped silently; partial writes do not abort parsing.
//! `last_session_at()` returns the file mtime of the most recent session file.

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::time::UNIX_EPOCH;

use serde::Deserialize;

use super::{TokenProvider, TokenUsage};

// ─────────────────────────────────────────────────────────────────────────────
// Raw NDJSON types
// ─────────────────────────────────────────────────────────────────────────────

/// Top-level NDJSON entry in a Codex session file.
#[derive(Debug, Deserialize)]
struct CodexEntry {
    #[serde(rename = "type")]
    entry_type: String,
    #[serde(default)]
    payload: serde_json::Value,
}

/// Token counts as emitted inside `info.last_token_usage` or
/// `info.total_token_usage`.
#[derive(Debug, Deserialize, Default, Clone, Copy)]
#[allow(
    clippy::struct_field_names,
    reason = "Field names mirror the Codex JSONL token_count schema"
)]
struct RawTokenCounts {
    #[serde(default)]
    input_tokens: u32,
    /// Primary cache-read field name.
    #[serde(default)]
    cached_input_tokens: u32,
    /// Alias used by some Codex builds; normalised to `cached_input_tokens`.
    #[serde(default)]
    cache_read_input_tokens: u32,
    #[serde(default)]
    output_tokens: u32,
    /// Informational only — already included in `output_tokens`. Do not add again.
    #[serde(default)]
    reasoning_output_tokens: u32,
    #[serde(default)]
    total_tokens: u32,
}

impl RawTokenCounts {
    /// Normalised cache-read value: prefer `cached_input_tokens` when both
    /// aliases are present (per spec: prefer the primary name).
    fn cache_read(self) -> u32 {
        if self.cached_input_tokens > 0 {
            self.cached_input_tokens
        } else {
            self.cache_read_input_tokens
        }
    }

    fn has_data(self) -> bool {
        self.input_tokens > 0 || self.output_tokens > 0 || self.cache_read() > 0
    }

    /// Saturating subtraction against a previous cumulative reading.
    fn saturating_sub(self, prev: Self) -> Self {
        Self {
            input_tokens: self.input_tokens.saturating_sub(prev.input_tokens),
            cached_input_tokens: self
                .cached_input_tokens
                .saturating_sub(prev.cached_input_tokens),
            cache_read_input_tokens: self
                .cache_read_input_tokens
                .saturating_sub(prev.cache_read_input_tokens),
            output_tokens: self.output_tokens.saturating_sub(prev.output_tokens),
            reasoning_output_tokens: self
                .reasoning_output_tokens
                .saturating_sub(prev.reasoning_output_tokens),
            total_tokens: self.total_tokens.saturating_sub(prev.total_tokens),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Path resolution
// ─────────────────────────────────────────────────────────────────────────────

/// Resolve the Codex base directory.
///
/// Returns `None` when neither `$CODEX_HOME` is set nor a home directory is
/// available. The returned path may not exist on disk.
fn resolve_codex_home() -> Option<PathBuf> {
    if let Ok(val) = std::env::var("CODEX_HOME") {
        if !val.is_empty() {
            return Some(PathBuf::from(val));
        }
    }
    dirs::home_dir().map(|h| h.join(".codex"))
}

// ─────────────────────────────────────────────────────────────────────────────
// Session file discovery
// ─────────────────────────────────────────────────────────────────────────────

/// Collect every `*.jsonl` path from the date-hierarchy sessions directory.
///
/// Layout: `sessions/YYYY/MM/DD/<id>.jsonl`. Traverses four levels deep
/// (YYYY → MM → DD → files) and skips entries that cannot be read.
fn collect_sessions_tree(sessions_dir: &std::path::Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let Ok(year_entries) = fs::read_dir(sessions_dir) else {
        return paths;
    };
    for year in year_entries.flatten() {
        let Ok(month_entries) = fs::read_dir(year.path()) else {
            continue;
        };
        for month in month_entries.flatten() {
            let Ok(day_entries) = fs::read_dir(month.path()) else {
                continue;
            };
            for day in day_entries.flatten() {
                // Each `day` entry is a directory like `11/`. Iterate its contents.
                let Ok(file_entries) = fs::read_dir(day.path()) else {
                    continue;
                };
                for file in file_entries.flatten() {
                    let p = file.path();
                    if p.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                        paths.push(p);
                    }
                }
            }
        }
    }
    paths
}

/// Collect every `*.jsonl` path from the flat `archived_sessions` directory.
fn collect_archived_sessions(archived_dir: &std::path::Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(archived_dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("jsonl"))
        .collect()
}

/// Return (`mtime_secs`, path) for the most recently modified session file, or
/// `None` if no session files exist under the Codex home.
fn most_recent_session(codex_home: &std::path::Path) -> Option<(u64, PathBuf)> {
    let mut all: Vec<PathBuf> = Vec::new();
    all.extend(collect_sessions_tree(&codex_home.join("sessions")));
    all.extend(collect_archived_sessions(
        &codex_home.join("archived_sessions"),
    ));

    all.into_iter()
        .filter_map(|p| {
            let mtime = fs::metadata(&p)
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs())?;
            Some((mtime, p))
        })
        .max_by_key(|(mtime, _)| *mtime)
}

// ─────────────────────────────────────────────────────────────────────────────
// NDJSON reader
// ─────────────────────────────────────────────────────────────────────────────

/// Accumulate token usage from a single Codex session JSONL file.
///
/// Uses `last_token_usage` (per-turn delta) when present. Falls back to
/// subtracting successive `total_token_usage` (cumulative) readings when
/// `last_token_usage` is absent. Skips malformed lines silently.
///
/// Returns `Ok(None)` when the file contains zero usable token-count entries.
fn read_session_file(path: &std::path::Path) -> anyhow::Result<Option<TokenUsage>> {
    let file = fs::File::open(path)?;
    let reader = BufReader::new(file);

    let mut accumulated = TokenUsage::default();
    let mut has_data = false;
    let mut prev_cumulative: Option<RawTokenCounts> = None;

    for line_result in reader.lines() {
        let line = line_result?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        // Skip malformed lines.
        let Ok(entry) = serde_json::from_str::<CodexEntry>(trimmed) else {
            continue;
        };

        if entry.entry_type != "event_msg" {
            continue;
        }

        // Check payload.type == "token_count".
        let payload_type = entry.payload.get("type").and_then(|v| v.as_str());
        if payload_type != Some("token_count") {
            continue;
        }

        let Some(info) = entry.payload.get("info") else {
            continue;
        };

        // Try to deserialise the delta first.
        let delta: Option<RawTokenCounts> = info
            .get("last_token_usage")
            .and_then(|v| serde_json::from_value(v.clone()).ok());

        // Deserialise cumulative total (used as fallback).
        let total: Option<RawTokenCounts> = info
            .get("total_token_usage")
            .and_then(|v| serde_json::from_value(v.clone()).ok());

        // Determine the delta to accumulate.
        let delta_counts = if let Some(d) = delta.filter(|d| d.has_data()) {
            // Preferred path: explicit per-turn delta.
            d
        } else if let Some(total_now) = total {
            // Fallback: synthesise delta from successive cumulative readings.
            let d = match prev_cumulative {
                Some(prev) => total_now.saturating_sub(prev),
                None => total_now,
            };
            prev_cumulative = Some(total_now);
            d
        } else {
            // Neither field present — skip this entry.
            continue;
        };

        // When using last_token_usage, still advance the cumulative tracker.
        if let Some(total_now) = total {
            prev_cumulative = Some(total_now);
        }

        if !delta_counts.has_data() {
            continue;
        }

        accumulated.prompt_tokens = accumulated
            .prompt_tokens
            .saturating_add(delta_counts.input_tokens);
        accumulated.completion_tokens = accumulated
            .completion_tokens
            .saturating_add(delta_counts.output_tokens);
        accumulated.cache_read_tokens = accumulated
            .cache_read_tokens
            .saturating_add(delta_counts.cache_read());
        // cache_creation_tokens: Codex does not emit this field; remains 0.

        has_data = true;
    }

    if has_data {
        Ok(Some(accumulated))
    } else {
        Ok(None)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// CodexProvider
// ─────────────────────────────────────────────────────────────────────────────

/// Reads token usage from the Codex CLI session store.
///
/// Resolves the Codex home directory from `$CODEX_HOME` or `~/.codex`,
/// scans the `sessions/` date hierarchy and `archived_sessions/` flat
/// directory, and parses the most recent JSONL session file.
///
/// When `session_file` is set, reads that file directly instead of
/// scanning for the most recent session. This supports multi-session
/// awareness where each terminal addresses its own session file.
#[derive(Debug)]
pub struct CodexProvider {
    /// Resolved Codex home directory. `None` when home resolution failed.
    codex_home: Option<PathBuf>,
    /// Optional explicit session file path. When set, this file is read
    /// directly instead of scanning for the most recent session.
    session_file: Option<PathBuf>,
}

impl CodexProvider {
    /// Create a new `CodexProvider` with the resolved Codex home directory.
    #[must_use]
    pub fn new() -> Self {
        Self {
            codex_home: resolve_codex_home(),
            session_file: None,
        }
    }

    /// Create a `CodexProvider` pointing at an explicit base directory.
    ///
    /// Useful for tests that supply a temporary directory.
    #[must_use]
    #[allow(dead_code)] // Used in tests and the lib crate; not called from the binary.
    pub fn with_home(codex_home: PathBuf) -> Self {
        Self {
            codex_home: Some(codex_home),
            session_file: None,
        }
    }

    /// Create a `CodexProvider` that reads a specific session file directly.
    ///
    /// When set, `session_usage()` reads this file instead of scanning for
    /// the most recent session under the Codex home directory.
    #[must_use]
    pub fn with_session_file(session_file: PathBuf) -> Self {
        Self {
            codex_home: resolve_codex_home(),
            session_file: Some(session_file),
        }
    }
}

impl Default for CodexProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl TokenProvider for CodexProvider {
    fn name(&self) -> &'static str {
        "codex"
    }

    /// Returns `true` when the Codex home directory exists on disk.
    fn is_available(&self) -> bool {
        self.codex_home
            .as_deref()
            .is_some_and(|p| p.exists() && p.is_dir())
    }

    /// Read token usage from the targeted or most recent Codex session file.
    ///
    /// When `session_file` is set, reads that file directly. Otherwise scans
    /// `$CODEX_HOME/sessions/` and `$CODEX_HOME/archived_sessions/`, picks
    /// the file with the highest mtime, and parses its NDJSON content.
    /// Returns `Ok(None)` when no session files exist or the file contains
    /// no usable token-count entries.
    fn session_usage(&self) -> anyhow::Result<Option<TokenUsage>> {
        if let Some(path) = &self.session_file {
            if path.exists() {
                return read_session_file(path);
            }
            return Ok(None);
        }
        let Some(home) = &self.codex_home else {
            return Ok(None);
        };
        if !home.exists() {
            return Ok(None);
        }
        let Some((_mtime, path)) = most_recent_session(home) else {
            return Ok(None);
        };
        read_session_file(&path)
    }

    /// Returns the mtime (seconds since Unix epoch) of the targeted or most
    /// recent Codex session file, or `None` when no session files are found.
    fn last_session_at(&self) -> Option<u64> {
        if let Some(path) = &self.session_file {
            let meta = fs::metadata(path).ok()?;
            let mtime = meta.modified().ok()?;
            return Some(mtime.duration_since(UNIX_EPOCH).ok()?.as_secs());
        }
        let home = self.codex_home.as_deref()?;
        if !home.exists() {
            return None;
        }
        let (mtime, _) = most_recent_session(home)?;
        Some(mtime)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::*;

    // ── Helpers ───────────────────────────────────────────────────────────────

    /// Write a JSONL session file at `base/sessions/YYYY/MM/DD/<name>.jsonl`.
    fn write_dated_session(base: &Path, name: &str, content: &str) -> PathBuf {
        let dir = base.join("sessions/2025/09/11");
        fs::create_dir_all(&dir).expect("create sessions dir");
        let path = dir.join(name);
        fs::write(&path, content).expect("write session file");
        path
    }

    /// Write a JSONL session file at `base/archived_sessions/<name>.jsonl`.
    fn write_archived_session(base: &Path, name: &str, content: &str) -> PathBuf {
        let dir = base.join("archived_sessions");
        fs::create_dir_all(&dir).expect("create archived dir");
        let path = dir.join(name);
        fs::write(&path, content).expect("write archived file");
        path
    }

    // ── path_resolution ───────────────────────────────────────────────────────

    #[test]
    #[allow(unsafe_code)] // Rust 2024: set_var/remove_var require unsafe; test-only env mutation.
    fn path_resolution_env_var_beats_home() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        // SAFETY: test-only; single-threaded context assumed for env mutation.
        unsafe { std::env::set_var("CODEX_HOME", dir.path()) };
        let provider = CodexProvider::new();
        // SAFETY: test-only cleanup.
        unsafe { std::env::remove_var("CODEX_HOME") };
        assert_eq!(provider.codex_home.as_deref(), Some(dir.path()));
    }

    #[test]
    #[allow(unsafe_code)] // Rust 2024: set_var/remove_var require unsafe; test-only env mutation.
    fn path_resolution_empty_env_var_falls_back_to_home() {
        // SAFETY: test-only; single-threaded context assumed for env mutation.
        unsafe { std::env::set_var("CODEX_HOME", "") };
        let provider = CodexProvider::new();
        // SAFETY: test-only cleanup.
        unsafe { std::env::remove_var("CODEX_HOME") };
        // Should resolve to home/.codex, not an empty path.
        let codex_home = provider.codex_home.expect("codex_home should be set");
        assert!(codex_home.ends_with(".codex"));
    }

    #[test]
    fn path_resolution_with_home_constructor() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let provider = CodexProvider::with_home(dir.path().to_path_buf());
        assert_eq!(provider.codex_home.as_deref(), Some(dir.path()));
    }

    // ── is_available ──────────────────────────────────────────────────────────

    #[test]
    fn is_available_false_when_dir_missing() {
        let provider = CodexProvider::with_home(PathBuf::from("/tmp/nonexistent-codex-annulus"));
        assert!(!provider.is_available());
    }

    #[test]
    fn is_available_true_when_dir_exists() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let provider = CodexProvider::with_home(dir.path().to_path_buf());
        assert!(provider.is_available());
    }

    // ── session_usage ─────────────────────────────────────────────────────────

    #[test]
    fn session_usage_returns_none_when_no_session_files() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let provider = CodexProvider::with_home(dir.path().to_path_buf());
        let result = provider.session_usage().expect("no error");
        assert!(result.is_none());
    }

    #[test]
    fn session_usage_returns_none_when_home_missing() {
        let provider = CodexProvider::with_home(PathBuf::from("/tmp/nonexistent-codex-annulus"));
        let result = provider.session_usage().expect("no error");
        assert!(result.is_none());
    }

    #[test]
    fn session_usage_aggregates_fixture() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        // Use the real fixture from the test fixtures directory.
        let fixture_path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/codex/sample-session.jsonl"
        );
        let content = fs::read_to_string(fixture_path).expect("fixture");
        write_dated_session(dir.path(), "sample-session.jsonl", &content);

        let provider = CodexProvider::with_home(dir.path().to_path_buf());
        let usage = provider
            .session_usage()
            .expect("no I/O error")
            .expect("should have usage");

        // Per fixture README gold expectations:
        // turn 1: input=1200, cached=200, output=500
        // turn 2: input=800, cached=100, output=300
        // aggregate: prompt=2000, completion=800, cache_read=300
        assert_eq!(
            usage.prompt_tokens, 2000,
            "prompt_tokens should sum both turns"
        );
        assert_eq!(
            usage.completion_tokens, 800,
            "completion_tokens should sum both turns"
        );
        assert_eq!(
            usage.cache_read_tokens, 300,
            "cache_read_tokens should sum both turns"
        );
        assert_eq!(
            usage.cache_creation_tokens, 0,
            "Codex does not emit cache_creation"
        );
    }

    #[test]
    fn session_usage_skips_malformed_lines() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let content = concat!(
            "not json at all\n",
            "{broken\n",
            "{\"timestamp\":\"2025-09-11T18:25:40.670Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\"model\":\"gpt-5\",\"total_token_usage\":{\"input_tokens\":500,\"cached_input_tokens\":50,\"output_tokens\":100,\"reasoning_output_tokens\":0,\"total_tokens\":600},\"last_token_usage\":{\"input_tokens\":500,\"cached_input_tokens\":50,\"output_tokens\":100,\"reasoning_output_tokens\":0,\"total_tokens\":600}}}}\n",
        );
        write_dated_session(dir.path(), "malformed.jsonl", content);

        let provider = CodexProvider::with_home(dir.path().to_path_buf());
        let usage = provider
            .session_usage()
            .expect("no I/O error")
            .expect("should have usage");
        assert_eq!(usage.prompt_tokens, 500);
        assert_eq!(usage.completion_tokens, 100);
        assert_eq!(usage.cache_read_tokens, 50);
    }

    #[test]
    fn session_usage_uses_archived_sessions() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let content = "{\"timestamp\":\"2025-09-11T18:25:40.670Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\"model\":\"gpt-5\",\"total_token_usage\":{\"input_tokens\":100,\"cached_input_tokens\":10,\"output_tokens\":50,\"reasoning_output_tokens\":0,\"total_tokens\":150},\"last_token_usage\":{\"input_tokens\":100,\"cached_input_tokens\":10,\"output_tokens\":50,\"reasoning_output_tokens\":0,\"total_tokens\":150}}}}\n";
        write_archived_session(dir.path(), "old-session.jsonl", content);

        let provider = CodexProvider::with_home(dir.path().to_path_buf());
        let usage = provider
            .session_usage()
            .expect("no I/O error")
            .expect("archived session found");
        assert_eq!(usage.prompt_tokens, 100);
        assert_eq!(usage.completion_tokens, 50);
    }

    #[test]
    fn session_usage_cumulative_fallback_when_last_absent() {
        // Test the cumulative-only path: no `last_token_usage`, only `total_token_usage`.
        // Reader must synthesise deltas by subtracting successive readings.
        let dir = tempfile::TempDir::new().expect("tempdir");
        let content = concat!(
            // Turn 1: cumulative total = 1200 input, 500 output. No previous reading → delta = same.
            "{\"timestamp\":\"2025-09-11T18:25:40.670Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\"model\":\"gpt-5\",\"total_token_usage\":{\"input_tokens\":1200,\"cached_input_tokens\":200,\"output_tokens\":500,\"reasoning_output_tokens\":0,\"total_tokens\":1700}}}}\n",
            // Turn 2: cumulative total = 2000 input, 800 output. Delta = 800 input, 300 output.
            "{\"timestamp\":\"2025-09-11T18:40:25.910Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\"model\":\"gpt-5\",\"total_token_usage\":{\"input_tokens\":2000,\"cached_input_tokens\":300,\"output_tokens\":800,\"reasoning_output_tokens\":0,\"total_tokens\":2800}}}}\n",
        );
        write_dated_session(dir.path(), "cumulative-only.jsonl", content);

        let provider = CodexProvider::with_home(dir.path().to_path_buf());
        let usage = provider
            .session_usage()
            .expect("no I/O error")
            .expect("should have usage");

        // Expected: turn 1 delta = (1200, 200, 500); turn 2 delta = (800, 100, 300).
        // Aggregate: prompt=2000, completion=800, cache_read=300.
        assert_eq!(usage.prompt_tokens, 2000, "cumulative fallback: prompt");
        assert_eq!(
            usage.completion_tokens, 800,
            "cumulative fallback: completion"
        );
        assert_eq!(
            usage.cache_read_tokens, 300,
            "cumulative fallback: cache_read"
        );
    }

    #[test]
    fn session_usage_cache_read_alias_normalised() {
        // `cache_read_input_tokens` alias should be accepted.
        let dir = tempfile::TempDir::new().expect("tempdir");
        let content = "{\"timestamp\":\"2025-09-11T18:25:40.670Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\"model\":\"gpt-5\",\"total_token_usage\":{\"input_tokens\":500,\"cache_read_input_tokens\":75,\"output_tokens\":200,\"reasoning_output_tokens\":0,\"total_tokens\":700},\"last_token_usage\":{\"input_tokens\":500,\"cache_read_input_tokens\":75,\"output_tokens\":200,\"reasoning_output_tokens\":0,\"total_tokens\":700}}}}\n";
        write_dated_session(dir.path(), "alias.jsonl", content);

        let provider = CodexProvider::with_home(dir.path().to_path_buf());
        let usage = provider
            .session_usage()
            .expect("no I/O error")
            .expect("should have usage");
        assert_eq!(
            usage.cache_read_tokens, 75,
            "alias cache_read_input_tokens must be accepted"
        );
    }

    #[test]
    fn session_usage_ignores_non_token_count_entries() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let content = concat!(
            "{\"timestamp\":\"2025-09-11T18:25:00.000Z\",\"type\":\"session_meta\",\"payload\":{\"id\":\"abc\",\"cwd\":\"/tmp\",\"model_provider\":\"openai\"}}\n",
            "{\"timestamp\":\"2025-09-11T18:25:30.000Z\",\"type\":\"turn_context\",\"payload\":{\"model\":\"gpt-5\",\"cwd\":\"/tmp\"}}\n",
            "{\"timestamp\":\"2025-09-11T18:25:40.000Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"user_message\",\"payload\":{\"text\":\"hello\"}}}\n",
        );
        write_dated_session(dir.path(), "no-tokens.jsonl", content);

        let provider = CodexProvider::with_home(dir.path().to_path_buf());
        let result = provider.session_usage().expect("no error");
        assert!(result.is_none(), "no token_count entries → None");
    }

    // ── last_session_at ───────────────────────────────────────────────────────

    #[test]
    fn last_session_at_returns_none_when_no_sessions() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let provider = CodexProvider::with_home(dir.path().to_path_buf());
        assert!(provider.last_session_at().is_none());
    }

    #[test]
    fn last_session_at_returns_mtime_for_session_file() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        write_dated_session(dir.path(), "s.jsonl", "{}");
        let provider = CodexProvider::with_home(dir.path().to_path_buf());
        let ts = provider.last_session_at();
        assert!(ts.is_some(), "should return mtime");
        // Sanity: file was just created, should be after 2020-01-01.
        assert!(
            ts.expect("ts should be Some") > 1_577_836_800,
            "mtime should be after 2020-01-01"
        );
    }

    #[test]
    fn last_session_at_returns_none_when_home_missing() {
        let provider = CodexProvider::with_home(PathBuf::from("/tmp/nonexistent-codex-annulus"));
        assert!(provider.last_session_at().is_none());
    }

    // ── session_file (session-scoped provider resolution) ────────────────────

    #[test]
    fn with_session_file_reads_specified_file() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let content = "{\"timestamp\":\"2025-09-11T18:25:40.670Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\"model\":\"gpt-5\",\"total_token_usage\":{\"input_tokens\":400,\"cached_input_tokens\":40,\"output_tokens\":150,\"reasoning_output_tokens\":0,\"total_tokens\":550},\"last_token_usage\":{\"input_tokens\":400,\"cached_input_tokens\":40,\"output_tokens\":150,\"reasoning_output_tokens\":0,\"total_tokens\":550}}}}\n";
        let session_path = dir.path().join("my-session.jsonl");
        fs::write(&session_path, content).expect("write session file");

        let provider = CodexProvider::with_session_file(session_path);
        let usage = provider
            .session_usage()
            .expect("no error")
            .expect("should have usage from session file");
        assert_eq!(usage.prompt_tokens, 400);
        assert_eq!(usage.completion_tokens, 150);
        assert_eq!(usage.cache_read_tokens, 40);
    }

    #[test]
    fn with_session_file_ignores_most_recent_global_session() {
        let dir = tempfile::TempDir::new().expect("tempdir");

        // Write a "global" session with different token counts.
        let global_content = "{\"timestamp\":\"2025-09-11T18:25:40.670Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\"model\":\"gpt-5\",\"total_token_usage\":{\"input_tokens\":9999,\"cached_input_tokens\":999,\"output_tokens\":8888,\"reasoning_output_tokens\":0,\"total_tokens\":18886},\"last_token_usage\":{\"input_tokens\":9999,\"cached_input_tokens\":999,\"output_tokens\":8888,\"reasoning_output_tokens\":0,\"total_tokens\":18886}}}}\n";
        write_dated_session(dir.path(), "global.jsonl", global_content);

        // Write a specific session file with smaller counts.
        let session_content = "{\"timestamp\":\"2025-09-11T18:25:40.670Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\"model\":\"gpt-5\",\"total_token_usage\":{\"input_tokens\":100,\"cached_input_tokens\":10,\"output_tokens\":50,\"reasoning_output_tokens\":0,\"total_tokens\":150},\"last_token_usage\":{\"input_tokens\":100,\"cached_input_tokens\":10,\"output_tokens\":50,\"reasoning_output_tokens\":0,\"total_tokens\":150}}}}\n";
        let session_path = dir.path().join("targeted-session.jsonl");
        fs::write(&session_path, session_content).expect("write session file");

        let provider = CodexProvider::with_session_file(session_path);
        let usage = provider
            .session_usage()
            .expect("no error")
            .expect("should have usage from targeted session");

        // Must read the targeted file, not the global one.
        assert_eq!(usage.prompt_tokens, 100, "should read targeted session, not global");
        assert_eq!(usage.completion_tokens, 50);
    }

    #[test]
    fn with_session_file_returns_none_when_file_missing() {
        let provider =
            CodexProvider::with_session_file(PathBuf::from("/tmp/nonexistent-codex-session.jsonl"));
        let result = provider.session_usage().expect("no error");
        assert!(result.is_none());
    }

    #[test]
    fn with_session_file_last_session_at_returns_file_mtime() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let session_path = dir.path().join("session.jsonl");
        fs::write(&session_path, "{}").expect("write session file");

        let provider = CodexProvider::with_session_file(session_path);
        let ts = provider.last_session_at();
        assert!(ts.is_some(), "should return mtime for existing session file");
        assert!(
            ts.expect("ts should be Some") > 1_577_836_800,
            "mtime should be after 2020-01-01"
        );
    }

    #[test]
    fn with_session_file_last_session_at_returns_none_when_missing() {
        let provider =
            CodexProvider::with_session_file(PathBuf::from("/tmp/nonexistent-codex-session.jsonl"));
        assert!(provider.last_session_at().is_none());
    }
}
