//! Gemini CLI token usage provider.
//!
//! Reads token usage from Gemini CLI session checkpoint files. Sessions are stored
//! as JSON arrays under `$GEMINI_HISTORY_DIR` (env override) or `~/.gemini/tmp/`.
//! Each UUID-named `.json` file is one complete conversation session; the CLI
//! rewrites the whole array on every turn.
//!
//! Path resolution order:
//! 1. `$GEMINI_HISTORY_DIR` environment variable, if set and non-empty.
//! 2. `~/.gemini/tmp/` as the default directory.
//!
//! `is_available()` returns `true` when the session directory exists on disk.
//! `session_usage()` finds the most recently modified `.json` file and reads it.
//! Token aggregation uses cumulative semantics: `prompt_tokens` comes from the
//! last model turn's `promptTokenCount`; `completion_tokens` is the sum of all
//! `candidatesTokenCount` values. Turns missing `usageMetadata` are skipped.
//! Malformed or truncated JSON returns `Ok(None)` rather than an error.
//! `last_session_at()` returns the mtime of the most recently modified session file.

use std::fs;
use std::path::PathBuf;
use std::time::UNIX_EPOCH;

use serde::Deserialize;

use super::{TokenProvider, TokenUsage};

// ─────────────────────────────────────────────────────────────────────────────
// Raw JSON types
// ─────────────────────────────────────────────────────────────────────────────

/// One element of the Gemini session JSON array.
#[derive(Debug, Deserialize)]
struct GeminiTurn {
    #[allow(dead_code)]
    role: String,
    #[serde(rename = "usageMetadata")]
    usage_metadata: Option<UsageMetadata>,
}

/// Token counts present on model turns.
#[derive(Debug, Deserialize, Clone, Copy)]
struct UsageMetadata {
    #[serde(rename = "promptTokenCount", default)]
    prompt_token_count: u32,
    #[serde(rename = "candidatesTokenCount", default)]
    candidates_token_count: u32,
}

// ─────────────────────────────────────────────────────────────────────────────
// Path resolution
// ─────────────────────────────────────────────────────────────────────────────

/// Resolve the Gemini session tmp directory.
///
/// Returns `None` when neither `$GEMINI_HISTORY_DIR` is set nor a home
/// directory is available. The returned path may not exist on disk.
fn resolve_gemini_tmp() -> Option<PathBuf> {
    if let Ok(val) = std::env::var("GEMINI_HISTORY_DIR") {
        if !val.is_empty() {
            return Some(PathBuf::from(val));
        }
    }
    dirs::home_dir().map(|h| h.join(".gemini").join("tmp"))
}

// ─────────────────────────────────────────────────────────────────────────────
// Session file discovery
// ─────────────────────────────────────────────────────────────────────────────

/// Return `(mtime_secs, path)` for the most recently modified `.json` file in
/// `dir`, or `None` if no `.json` files exist.
fn most_recent_session(dir: &std::path::Path) -> Option<(u64, PathBuf)> {
    let entries = fs::read_dir(dir).ok()?;
    entries
        .flatten()
        .filter_map(|entry| {
            let p = entry.path();
            if p.extension().and_then(|e| e.to_str()) != Some("json") {
                return None;
            }
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
// JSON reader
// ─────────────────────────────────────────────────────────────────────────────

/// Accumulate token usage from a single Gemini session JSON file.
///
/// Uses cumulative semantics: `prompt_tokens` = last model turn's
/// `promptTokenCount`; `completion_tokens` = sum of all `candidatesTokenCount`.
/// Turns missing `usageMetadata` are skipped. Malformed JSON returns `Ok(None)`.
///
/// Returns `Ok(None)` when the file contains zero usable token entries.
fn read_session_file(path: &std::path::Path) -> anyhow::Result<Option<TokenUsage>> {
    let content = fs::read_to_string(path)?;

    // Treat truncated or malformed JSON as "no data" per the edge-case spec.
    let turns: Vec<GeminiTurn> = match serde_json::from_str(&content) {
        Ok(t) => t,
        Err(_) => return Ok(None),
    };

    let mut last_prompt_tokens: Option<u32> = None;
    let mut completion_tokens_sum: u32 = 0;
    let mut has_data = false;

    for turn in turns {
        let Some(meta) = turn.usage_metadata else {
            continue;
        };
        last_prompt_tokens = Some(meta.prompt_token_count);
        completion_tokens_sum = completion_tokens_sum.saturating_add(meta.candidates_token_count);
        has_data = true;
    }

    if !has_data {
        return Ok(None);
    }

    Ok(Some(TokenUsage {
        prompt_tokens: last_prompt_tokens.unwrap_or(0),
        completion_tokens: completion_tokens_sum,
        cache_read_tokens: 0,
        cache_creation_tokens: 0,
    }))
}

// ─────────────────────────────────────────────────────────────────────────────
// GeminiProvider
// ─────────────────────────────────────────────────────────────────────────────

/// Reads token usage from the Gemini CLI session store.
///
/// Resolves the session directory from `$GEMINI_HISTORY_DIR` or
/// `~/.gemini/tmp/`, scans for the most recent `.json` session file, and
/// parses its JSON array using cumulative token-count semantics.
///
/// When `session_file` is set, reads that file directly instead of
/// scanning for the most recent session. This supports multi-session
/// awareness where each terminal addresses its own session file.
#[derive(Debug)]
pub struct GeminiProvider {
    /// Resolved Gemini session tmp directory. `None` when home resolution failed.
    tmp_dir: Option<PathBuf>,
    /// Optional explicit session file path. When set, this file is read
    /// directly instead of scanning for the most recent session.
    session_file: Option<PathBuf>,
}

impl GeminiProvider {
    /// Create a new `GeminiProvider` with the resolved session directory.
    #[must_use]
    pub fn new() -> Self {
        Self {
            tmp_dir: resolve_gemini_tmp(),
            session_file: None,
        }
    }

    /// Create a `GeminiProvider` pointing at an explicit session directory.
    ///
    /// Useful for tests that supply a temporary directory.
    #[must_use]
    #[allow(dead_code)] // Used in tests; not called from the binary.
    pub fn with_tmp_dir(tmp_dir: PathBuf) -> Self {
        Self {
            tmp_dir: Some(tmp_dir),
            session_file: None,
        }
    }

    /// Create a `GeminiProvider` that reads a specific session file directly.
    ///
    /// When set, `session_usage()` reads this file instead of scanning for
    /// the most recent session in the tmp directory.
    #[must_use]
    pub fn with_session_file(session_file: PathBuf) -> Self {
        Self {
            tmp_dir: resolve_gemini_tmp(),
            session_file: Some(session_file),
        }
    }
}

impl Default for GeminiProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl TokenProvider for GeminiProvider {
    fn name(&self) -> &'static str {
        "gemini"
    }

    /// Returns `true` when the Gemini session directory exists on disk.
    fn is_available(&self) -> bool {
        self.tmp_dir
            .as_deref()
            .is_some_and(|p| p.exists() && p.is_dir())
    }

    /// Read token usage from the targeted or most recent Gemini session file.
    ///
    /// When `session_file` is set, reads that file directly. Otherwise finds
    /// the `.json` file with the highest mtime in the session directory,
    /// parses the JSON array, and accumulates token counts using cumulative
    /// semantics. Returns `Ok(None)` when no session files exist or the file
    /// contains no usable token entries.
    fn session_usage(&self) -> anyhow::Result<Option<TokenUsage>> {
        if let Some(path) = &self.session_file {
            if path.exists() {
                return read_session_file(path);
            }
            return Ok(None);
        }
        let Some(dir) = &self.tmp_dir else {
            return Ok(None);
        };
        if !dir.exists() {
            return Ok(None);
        }
        let Some((_mtime, path)) = most_recent_session(dir) else {
            return Ok(None);
        };
        read_session_file(&path)
    }

    /// Returns the mtime (seconds since Unix epoch) of the targeted or most
    /// recent Gemini session file, or `None` when no session files are found.
    fn last_session_at(&self) -> Option<u64> {
        if let Some(path) = &self.session_file {
            let meta = fs::metadata(path).ok()?;
            let mtime = meta.modified().ok()?;
            return Some(mtime.duration_since(UNIX_EPOCH).ok()?.as_secs());
        }
        let dir = self.tmp_dir.as_deref()?;
        if !dir.exists() {
            return None;
        }
        let (mtime, _) = most_recent_session(dir)?;
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

    /// Write a `.json` session file at `dir/<name>.json`.
    fn write_session(dir: &Path, name: &str, content: &str) -> PathBuf {
        fs::create_dir_all(dir).expect("create session dir");
        let path = dir.join(name);
        fs::write(&path, content).expect("write session file");
        path
    }

    // ── path_resolution ───────────────────────────────────────────────────────

    #[test]
    #[allow(unsafe_code)] // Rust 2024: set_var/remove_var require unsafe; test-only env mutation.
    fn path_resolution_env_var_beats_home() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        // SAFETY: test-only; single-threaded context assumed for env mutation.
        unsafe { std::env::set_var("GEMINI_HISTORY_DIR", dir.path()) };
        let provider = GeminiProvider::new();
        // SAFETY: test-only cleanup.
        unsafe { std::env::remove_var("GEMINI_HISTORY_DIR") };
        assert_eq!(provider.tmp_dir.as_deref(), Some(dir.path()));
    }

    #[test]
    #[allow(unsafe_code)] // Rust 2024: set_var/remove_var require unsafe; test-only env mutation.
    fn path_resolution_empty_env_var_falls_back_to_home() {
        // SAFETY: test-only; single-threaded context assumed for env mutation.
        unsafe { std::env::set_var("GEMINI_HISTORY_DIR", "") };
        let provider = GeminiProvider::new();
        // SAFETY: test-only cleanup.
        unsafe { std::env::remove_var("GEMINI_HISTORY_DIR") };
        // Should resolve to home/.gemini/tmp, not an empty path.
        let tmp_dir = provider.tmp_dir.expect("tmp_dir should be set");
        assert!(
            tmp_dir.ends_with(".gemini/tmp"),
            "expected .gemini/tmp, got {tmp_dir:?}"
        );
    }

    #[test]
    fn path_resolution_with_tmp_dir_constructor() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let provider = GeminiProvider::with_tmp_dir(dir.path().to_path_buf());
        assert_eq!(provider.tmp_dir.as_deref(), Some(dir.path()));
    }

    // ── is_available ──────────────────────────────────────────────────────────

    #[test]
    fn is_available_false_when_dir_missing() {
        let provider =
            GeminiProvider::with_tmp_dir(PathBuf::from("/tmp/nonexistent-gemini-annulus"));
        assert!(!provider.is_available());
    }

    #[test]
    fn is_available_true_when_dir_exists() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let provider = GeminiProvider::with_tmp_dir(dir.path().to_path_buf());
        assert!(provider.is_available());
    }

    // ── session_usage ─────────────────────────────────────────────────────────

    #[test]
    fn session_usage_returns_none_when_dir_missing() {
        let provider =
            GeminiProvider::with_tmp_dir(PathBuf::from("/tmp/nonexistent-gemini-annulus"));
        let result = provider.session_usage().expect("no error");
        assert!(result.is_none());
    }

    #[test]
    fn session_usage_returns_none_when_no_json_files() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let provider = GeminiProvider::with_tmp_dir(dir.path().to_path_buf());
        let result = provider.session_usage().expect("no error");
        assert!(result.is_none());
    }

    #[test]
    fn session_usage_returns_none_for_empty_array() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        write_session(dir.path(), "empty.json", "[]");
        let provider = GeminiProvider::with_tmp_dir(dir.path().to_path_buf());
        let result = provider.session_usage().expect("no error");
        assert!(result.is_none());
    }

    #[test]
    fn session_usage_returns_none_for_malformed_json() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        write_session(dir.path(), "broken.json", "{not valid json");
        let provider = GeminiProvider::with_tmp_dir(dir.path().to_path_buf());
        let result = provider.session_usage().expect("no error");
        assert!(result.is_none());
    }

    #[test]
    fn session_usage_aggregates_fixture() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        // Use the real fixture from the test fixtures directory.
        let fixture_path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/gemini/sample-session.json"
        );
        let content = fs::read_to_string(fixture_path).expect("fixture");
        write_session(dir.path(), "sample-session.json", &content);

        let provider = GeminiProvider::with_tmp_dir(dir.path().to_path_buf());
        let usage = provider
            .session_usage()
            .expect("no I/O error")
            .expect("should have usage");

        // Per fixture README gold expectations:
        // prompt_tokens: 1534 (last model turn's promptTokenCount)
        // completion_tokens: 87 + 203 + 411 = 701 (sum of all candidatesTokenCount)
        // cache_read_tokens: 0, cache_creation_tokens: 0
        assert_eq!(
            usage.prompt_tokens, 1534,
            "prompt_tokens = last model turn promptTokenCount"
        );
        assert_eq!(
            usage.completion_tokens, 701,
            "completion_tokens = sum of all candidatesTokenCount"
        );
        assert_eq!(
            usage.cache_read_tokens, 0,
            "Gemini does not emit cache_read"
        );
        assert_eq!(
            usage.cache_creation_tokens, 0,
            "Gemini does not emit cache_creation"
        );
    }

    #[test]
    fn session_usage_skips_turns_without_usage_metadata() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        // Only the last turn has usageMetadata; the first model turn is missing it.
        let content = r#"[
            {"role":"user","parts":[{"text":"hello"}]},
            {"role":"model","parts":[{"text":"hi"}]},
            {"role":"user","parts":[{"text":"more"}]},
            {"role":"model","parts":[{"text":"reply"}],"usageMetadata":{"promptTokenCount":500,"candidatesTokenCount":100,"totalTokenCount":600}}
        ]"#;
        write_session(dir.path(), "partial.json", content);

        let provider = GeminiProvider::with_tmp_dir(dir.path().to_path_buf());
        let usage = provider
            .session_usage()
            .expect("no I/O error")
            .expect("should have usage");

        assert_eq!(usage.prompt_tokens, 500);
        assert_eq!(usage.completion_tokens, 100);
    }

    #[test]
    fn session_usage_cumulative_semantics_last_prompt_wins() {
        // Verify that prompt_tokens uses the LAST model turn, not a sum.
        let dir = tempfile::TempDir::new().expect("tempdir");
        let content = r#"[
            {"role":"user","parts":[{"text":"q1"}]},
            {"role":"model","parts":[{"text":"a1"}],"usageMetadata":{"promptTokenCount":100,"candidatesTokenCount":50,"totalTokenCount":150}},
            {"role":"user","parts":[{"text":"q2"}]},
            {"role":"model","parts":[{"text":"a2"}],"usageMetadata":{"promptTokenCount":300,"candidatesTokenCount":75,"totalTokenCount":375}}
        ]"#;
        write_session(dir.path(), "cumulative.json", content);

        let provider = GeminiProvider::with_tmp_dir(dir.path().to_path_buf());
        let usage = provider
            .session_usage()
            .expect("no I/O error")
            .expect("should have usage");

        // prompt_tokens = last turn's promptTokenCount = 300 (NOT 100+300=400)
        assert_eq!(
            usage.prompt_tokens, 300,
            "prompt_tokens must be last turn, not sum"
        );
        // completion_tokens = 50 + 75 = 125 (sum across all turns)
        assert_eq!(
            usage.completion_tokens, 125,
            "completion_tokens must sum all turns"
        );
    }

    // ── last_session_at ───────────────────────────────────────────────────────

    #[test]
    fn last_session_at_returns_none_when_no_sessions() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let provider = GeminiProvider::with_tmp_dir(dir.path().to_path_buf());
        assert!(provider.last_session_at().is_none());
    }

    #[test]
    fn last_session_at_returns_none_when_dir_missing() {
        let provider =
            GeminiProvider::with_tmp_dir(PathBuf::from("/tmp/nonexistent-gemini-annulus"));
        assert!(provider.last_session_at().is_none());
    }

    #[test]
    fn last_session_at_returns_mtime_for_session_file() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        write_session(dir.path(), "s.json", "[]");
        let provider = GeminiProvider::with_tmp_dir(dir.path().to_path_buf());
        let ts = provider.last_session_at();
        assert!(ts.is_some(), "should return mtime");
        // Sanity: file was just created, should be after 2020-01-01.
        assert!(
            ts.expect("ts should be Some") > 1_577_836_800,
            "mtime should be after 2020-01-01"
        );
    }

    // ── session_file (session-scoped provider resolution) ────────────────────

    #[test]
    fn with_session_file_reads_specified_file() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let content = r#"[
            {"role":"user","parts":[{"text":"hello"}]},
            {"role":"model","parts":[{"text":"world"}],"usageMetadata":{"promptTokenCount":250,"candidatesTokenCount":80,"totalTokenCount":330}}
        ]"#;
        let session_path = dir.path().join("my-session.json");
        fs::write(&session_path, content).expect("write session file");

        let provider = GeminiProvider::with_session_file(session_path);
        let usage = provider
            .session_usage()
            .expect("no error")
            .expect("should have usage from session file");
        assert_eq!(usage.prompt_tokens, 250);
        assert_eq!(usage.completion_tokens, 80);
    }

    #[test]
    fn with_session_file_ignores_most_recent_global_session() {
        let dir = tempfile::TempDir::new().expect("tempdir");

        // Write a "global" session in the tmp dir with large token counts.
        let global_dir = dir.path().join("global-tmp");
        let global_content = r#"[
            {"role":"user","parts":[{"text":"q"}]},
            {"role":"model","parts":[{"text":"a"}],"usageMetadata":{"promptTokenCount":9999,"candidatesTokenCount":8888,"totalTokenCount":18887}}
        ]"#;
        write_session(&global_dir, "global.json", global_content);

        // Write a targeted session file with smaller counts.
        let session_content = r#"[
            {"role":"user","parts":[{"text":"q"}]},
            {"role":"model","parts":[{"text":"a"}],"usageMetadata":{"promptTokenCount":100,"candidatesTokenCount":50,"totalTokenCount":150}}
        ]"#;
        let session_path = dir.path().join("targeted-session.json");
        fs::write(&session_path, session_content).expect("write session file");

        let provider = GeminiProvider::with_session_file(session_path);
        let usage = provider
            .session_usage()
            .expect("no error")
            .expect("should have usage from targeted session");

        assert_eq!(
            usage.prompt_tokens, 100,
            "should read targeted session, not global"
        );
        assert_eq!(usage.completion_tokens, 50);
    }

    #[test]
    fn with_session_file_returns_none_when_file_missing() {
        let provider = GeminiProvider::with_session_file(PathBuf::from(
            "/tmp/nonexistent-gemini-session.json",
        ));
        let result = provider.session_usage().expect("no error");
        assert!(result.is_none());
    }

    #[test]
    fn with_session_file_last_session_at_returns_file_mtime() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let session_path = dir.path().join("session.json");
        fs::write(&session_path, "[]").expect("write session file");

        let provider = GeminiProvider::with_session_file(session_path);
        let ts = provider.last_session_at();
        assert!(
            ts.is_some(),
            "should return mtime for existing session file"
        );
        assert!(
            ts.expect("ts should be Some") > 1_577_836_800,
            "mtime should be after 2020-01-01"
        );
    }

    #[test]
    fn with_session_file_last_session_at_returns_none_when_missing() {
        let provider = GeminiProvider::with_session_file(PathBuf::from(
            "/tmp/nonexistent-gemini-session.json",
        ));
        assert!(provider.last_session_at().is_none());
    }
}
