//! Gemini usage scanner.
//!
//! Reads JSON session files from the Gemini CLI session store and produces one
//! [`UsageRow`] per `(date, model)` pair. Reuses the session-file discovery and
//! JSON parsing conventions from [`crate::providers::gemini`] rather than
//! reimplementing them.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::Deserialize;

use super::{UsageRow, UsageScanner};

// ─────────────────────────────────────────────────────────────────────────────
// Cost estimation
// ─────────────────────────────────────────────────────────────────────────────

/// Estimate USD cost for Gemini token counts.
///
/// Uses Gemini 1.5 Pro pricing as a rough proxy: $3.50/M input, $10.50/M output.
fn estimate_cost(prompt_tokens: u64, completion_tokens: u64) -> f64 {
    const PROMPT_RATE: f64 = 3.5 / 1_000_000.0;
    const COMPLETION_RATE: f64 = 10.5 / 1_000_000.0;
    #[allow(clippy::cast_precision_loss)]
    let p = prompt_tokens as f64;
    #[allow(clippy::cast_precision_loss)]
    let c = completion_tokens as f64;
    p * PROMPT_RATE + c * COMPLETION_RATE
}

// ─────────────────────────────────────────────────────────────────────────────
// Raw JSON types (mirrors providers/gemini.rs)
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct GeminiTurn {
    #[allow(dead_code)]
    role: String,
    #[serde(rename = "usageMetadata")]
    usage_metadata: Option<UsageMetadata>,
}

#[derive(Debug, Deserialize, Clone, Copy)]
struct UsageMetadata {
    #[serde(rename = "promptTokenCount", default)]
    prompt_token_count: u64,
    #[serde(rename = "candidatesTokenCount", default)]
    candidates_token_count: u64,
}

// ─────────────────────────────────────────────────────────────────────────────
// Date from file mtime
// ─────────────────────────────────────────────────────────────────────────────

fn date_from_mtime(path: &Path) -> String {
    let secs = fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());
    epoch_secs_to_date(secs)
}

/// Convert a Unix timestamp (seconds) to a `YYYY-MM-DD` string (UTC).
pub(crate) fn epoch_secs_to_date(secs: u64) -> String {
    let days = secs / 86_400;
    #[allow(clippy::cast_possible_wrap)]
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    #[allow(clippy::cast_sign_loss)]
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    #[allow(clippy::cast_possible_wrap)]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

// ─────────────────────────────────────────────────────────────────────────────
// Session file discovery
// ─────────────────────────────────────────────────────────────────────────────

fn collect_session_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
        .collect()
}

// ─────────────────────────────────────────────────────────────────────────────
// Per-file scanning
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Default)]
struct Bucket {
    last_prompt: u64,
    completion_sum: u64,
}

/// Scan a single Gemini session JSON file.
///
/// Uses the same cumulative semantics as the [`crate::providers::gemini`]
/// provider: `prompt_tokens` = last model turn's `promptTokenCount`;
/// `completion_tokens` = sum of all `candidatesTokenCount`. The model label
/// defaults to `"gemini"` since Gemini session files do not carry a model field.
fn scan_session_file(path: &Path, runtime_id: &str) -> Vec<UsageRow> {
    let Ok(content) = fs::read_to_string(path) else {
        return Vec::new();
    };

    let turns: Vec<GeminiTurn> = match serde_json::from_str(&content) {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };

    // Gemini files carry no date per-turn; use file mtime for the date.
    let date = date_from_mtime(path);
    // Gemini files carry no model field; label rows as "gemini".
    let model = "gemini".to_owned();

    let mut buckets: HashMap<(String, String), Bucket> = HashMap::new();

    for turn in turns {
        let Some(meta) = turn.usage_metadata else {
            continue;
        };
        let bucket = buckets.entry((date.clone(), model.clone())).or_default();
        bucket.last_prompt = meta.prompt_token_count;
        bucket.completion_sum = bucket
            .completion_sum
            .saturating_add(meta.candidates_token_count);
    }

    let mut rows: Vec<UsageRow> = buckets
        .into_iter()
        .filter_map(|((date, model), bucket)| {
            if bucket.last_prompt == 0 && bucket.completion_sum == 0 {
                return None;
            }
            let cost = estimate_cost(bucket.last_prompt, bucket.completion_sum);
            Some(UsageRow {
                runtime_id: runtime_id.to_owned(),
                date,
                model,
                prompt_tokens: bucket.last_prompt,
                completion_tokens: bucket.completion_sum,
                cache_tokens: 0,
                cost_usd: cost,
            })
        })
        .collect();
    rows.sort_by(|a, b| a.date.cmp(&b.date).then(a.model.cmp(&b.model)));
    rows
}

// ─────────────────────────────────────────────────────────────────────────────
// GeminiScanner
// ─────────────────────────────────────────────────────────────────────────────

/// Scans all Gemini CLI session files under a session directory and emits one
/// [`UsageRow`] per `(date, model)` pair per session file.
///
/// The `runtime_path` passed to [`UsageScanner::scan`] should be the Gemini
/// session directory (e.g. `~/.gemini/tmp`). Each UUID-named `.json` file is one
/// complete session; the file stem is used as the `runtime_id`.
///
/// # Example
///
/// ```no_run
/// use annulus::usage::gemini::GeminiScanner;
/// use annulus::usage::UsageScanner;
/// use std::path::Path;
///
/// let scanner = GeminiScanner;
/// let rows = scanner.scan(Path::new("/home/user/.gemini/tmp"));
/// println!("found {} rows", rows.len());
/// ```
pub struct GeminiScanner;

impl UsageScanner for GeminiScanner {
    fn scan(&self, runtime_path: &Path) -> Vec<UsageRow> {
        if !runtime_path.exists() {
            return Vec::new();
        }

        let paths = collect_session_files(runtime_path);
        let mut all_rows: Vec<UsageRow> = Vec::new();

        for path in &paths {
            let rid = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_owned();
            all_rows.extend(scan_session_file(path, &rid));
        }

        all_rows.sort_by(|a, b| {
            a.date
                .cmp(&b.date)
                .then(a.model.cmp(&b.model))
                .then(a.runtime_id.cmp(&b.runtime_id))
        });
        all_rows
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

    fn write_session(dir: &Path, name: &str, content: &str) -> PathBuf {
        fs::create_dir_all(dir).expect("create dir");
        let path = dir.join(name);
        fs::write(&path, content).expect("write session file");
        path
    }

    #[test]
    fn gemini_scanner_uses_fixture() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let fixture = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/gemini/sample-session.json"
        );
        let content = fs::read_to_string(fixture).expect("fixture");
        write_session(dir.path(), "sample-session.json", &content);

        let scanner = GeminiScanner;
        let rows = scanner.scan(dir.path());

        assert!(!rows.is_empty(), "should produce rows from fixture");
        let row = &rows[0];
        assert_eq!(row.model, "gemini");
        // prompt_tokens = last model turn's promptTokenCount = 1534
        assert_eq!(
            row.prompt_tokens, 1534,
            "prompt = last model turn promptTokenCount"
        );
        // completion = 87 + 203 + 411 = 701
        assert_eq!(row.completion_tokens, 701, "completion = sum of all turns");
        assert_eq!(row.cache_tokens, 0);
        assert!(row.cost_usd > 0.0);
    }

    #[test]
    fn gemini_scanner_returns_empty_for_missing_dir() {
        let scanner = GeminiScanner;
        let rows = scanner.scan(Path::new("/tmp/nonexistent-gemini-scanner-annulus"));
        assert!(rows.is_empty(), "missing dir → empty result, no panic");
    }

    #[test]
    fn gemini_scanner_returns_empty_for_empty_array() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        write_session(dir.path(), "empty.json", "[]");
        let scanner = GeminiScanner;
        let rows = scanner.scan(dir.path());
        assert!(rows.is_empty(), "empty session → no rows");
    }

    #[test]
    fn gemini_scanner_returns_empty_for_malformed_json() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        write_session(dir.path(), "broken.json", "{not valid");
        let scanner = GeminiScanner;
        let rows = scanner.scan(dir.path());
        assert!(rows.is_empty(), "malformed json → no rows, no panic");
    }

    #[test]
    fn gemini_scanner_multiple_sessions_produce_separate_rows() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let content_a = r#"[
            {"role":"user","parts":[{"text":"q"}]},
            {"role":"model","parts":[{"text":"a"}],"usageMetadata":{"promptTokenCount":100,"candidatesTokenCount":50,"totalTokenCount":150}}
        ]"#;
        let content_b = r#"[
            {"role":"user","parts":[{"text":"q"}]},
            {"role":"model","parts":[{"text":"a"}],"usageMetadata":{"promptTokenCount":200,"candidatesTokenCount":80,"totalTokenCount":280}}
        ]"#;
        write_session(dir.path(), "session-a.json", content_a);
        write_session(dir.path(), "session-b.json", content_b);

        let scanner = GeminiScanner;
        let rows = scanner.scan(dir.path());
        // Two separate session files → two rows (different runtime_ids).
        assert_eq!(rows.len(), 2, "two session files → two rows");
        let rid_a = rows.iter().find(|r| r.runtime_id == "session-a");
        let rid_b = rows.iter().find(|r| r.runtime_id == "session-b");
        assert!(rid_a.is_some(), "session-a runtime_id present");
        assert!(rid_b.is_some(), "session-b runtime_id present");
    }

    #[test]
    fn epoch_secs_to_date_known_values() {
        assert_eq!(epoch_secs_to_date(0), "1970-01-01");
        assert_eq!(epoch_secs_to_date(1_735_689_600), "2025-01-01");
        assert_eq!(epoch_secs_to_date(1_775_894_400), "2026-04-11");
    }
}
