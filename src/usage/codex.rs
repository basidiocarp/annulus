//! Codex usage scanner.
//!
//! Reads NDJSON session files from the Codex CLI session store and produces one
//! [`UsageRow`] per `(date, model)` pair. Reuses the session-file discovery
//! logic from [`crate::providers::codex`] (path resolution, tree walk, and
//! archived-sessions collection) while layering per-date-model aggregation on
//! top of it.

use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::Deserialize;

use super::{UsageRow, UsageScanner};

// ─────────────────────────────────────────────────────────────────────────────
// Cost estimation
// ─────────────────────────────────────────────────────────────────────────────

/// Estimate USD cost for Codex token counts.
///
/// Uses GPT-4o pricing as a rough proxy: $2.50/M input, $10.00/M output.
fn estimate_cost(prompt_tokens: u64, completion_tokens: u64) -> f64 {
    const PROMPT_RATE: f64 = 2.5 / 1_000_000.0;
    const COMPLETION_RATE: f64 = 10.0 / 1_000_000.0;
    #[allow(clippy::cast_precision_loss)]
    let p = prompt_tokens as f64;
    #[allow(clippy::cast_precision_loss)]
    let c = completion_tokens as f64;
    p * PROMPT_RATE + c * COMPLETION_RATE
}

// ─────────────────────────────────────────────────────────────────────────────
// Raw NDJSON types (subset needed for scanner; mirrors providers/codex.rs)
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct CodexEntry {
    #[serde(rename = "type")]
    entry_type: String,
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default)]
    payload: serde_json::Value,
}

#[allow(
    clippy::struct_field_names,
    reason = "Field names mirror the Codex JSONL token_count schema"
)]
#[derive(Debug, Deserialize, Default, Clone, Copy)]
struct RawTokenCounts {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    cached_input_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
}

impl RawTokenCounts {
    fn cache_read(self) -> u64 {
        if self.cached_input_tokens > 0 {
            self.cached_input_tokens
        } else {
            self.cache_read_input_tokens
        }
    }

    fn has_data(self) -> bool {
        self.input_tokens > 0 || self.output_tokens > 0 || self.cache_read() > 0
    }

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
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Date extraction
// ─────────────────────────────────────────────────────────────────────────────

fn date_from_timestamp(ts: &str) -> Option<String> {
    if ts.len() >= 10 {
        Some(ts[..10].to_owned())
    } else {
        None
    }
}

fn date_from_path(path: &Path) -> Option<String> {
    // Sessions live under YYYY/MM/DD/<id>.jsonl — walk upwards to extract.
    let mut parts = path.components().rev();
    parts.next(); // filename
    let day = parts.next()?.as_os_str().to_str()?;
    let month = parts.next()?.as_os_str().to_str()?;
    let year = parts.next()?.as_os_str().to_str()?;
    // Validate rough shape.
    if year.len() == 4 && month.len() == 2 && day.len() == 2 {
        Some(format!("{year}-{month}-{day}"))
    } else {
        None
    }
}

fn date_from_mtime(path: &Path) -> Option<String> {
    let mtime = fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())?;
    Some(epoch_secs_to_date(mtime))
}

/// Convert a Unix timestamp (seconds) to a `YYYY-MM-DD` string (UTC).
pub(crate) fn epoch_secs_to_date(secs: u64) -> String {
    // Days since Unix epoch.
    let days = secs / 86_400;
    // Howard Hinnant's civil_from_days algorithm.
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
// Session file scanning
// ─────────────────────────────────────────────────────────────────────────────

#[allow(
    clippy::struct_field_names,
    reason = "Field names are token-category descriptors, not redundant postfixes"
)]
#[derive(Default)]
struct Bucket {
    prompt_tokens: u64,
    completion_tokens: u64,
    cache_tokens: u64,
}

/// Scan a single Codex session JSONL file and accumulate per-(date, model) rows.
///
/// Falls back from entry timestamps → path hierarchy date → file mtime for the
/// date, and to `"codex"` when the model field cannot be determined.
fn scan_session_file(path: &Path, runtime_id: &str) -> Vec<UsageRow> {
    let fallback_date = date_from_path(path)
        .or_else(|| date_from_mtime(path))
        .unwrap_or_else(|| "1970-01-01".to_owned());

    let Ok(file) = fs::File::open(path) else {
        return Vec::new();
    };
    let reader = BufReader::new(file);

    let mut buckets: HashMap<(String, String), Bucket> = HashMap::new();
    let mut prev_cumulative: Option<RawTokenCounts> = None;
    let mut current_model = "codex".to_owned();

    for line_result in reader.lines() {
        let Ok(line) = line_result else { continue };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(entry) = serde_json::from_str::<CodexEntry>(trimmed) else {
            continue;
        };

        // Capture the most recently seen model from turn_context entries.
        if entry.entry_type == "turn_context" {
            if let Some(m) = entry.payload.get("model").and_then(|v| v.as_str()) {
                if !m.is_empty() {
                    m.clone_into(&mut current_model);
                }
            }
        }

        if entry.entry_type != "event_msg" {
            continue;
        }

        let payload_type = entry.payload.get("type").and_then(|v| v.as_str());
        if payload_type != Some("token_count") {
            continue;
        }

        let Some(info) = entry.payload.get("info") else {
            continue;
        };

        // Model may be on info.model.
        if let Some(m) = info.get("model").and_then(|v| v.as_str()) {
            if !m.is_empty() {
                m.clone_into(&mut current_model);
            }
        }

        let delta: Option<RawTokenCounts> = info
            .get("last_token_usage")
            .and_then(|v| serde_json::from_value(v.clone()).ok());

        let total: Option<RawTokenCounts> = info
            .get("total_token_usage")
            .and_then(|v| serde_json::from_value(v.clone()).ok());

        let delta_counts = if let Some(d) = delta.filter(|d| d.has_data()) {
            d
        } else if let Some(total_now) = total {
            let d = match prev_cumulative {
                Some(prev) => total_now.saturating_sub(prev),
                None => total_now,
            };
            prev_cumulative = Some(total_now);
            d
        } else {
            continue;
        };

        if let Some(total_now) = total {
            prev_cumulative = Some(total_now);
        }

        if !delta_counts.has_data() {
            continue;
        }

        // Determine the date from the entry timestamp or fall back.
        let date = entry
            .timestamp
            .as_deref()
            .and_then(date_from_timestamp)
            .unwrap_or_else(|| fallback_date.clone());

        let bucket = buckets.entry((date, current_model.clone())).or_default();
        bucket.prompt_tokens = bucket
            .prompt_tokens
            .saturating_add(delta_counts.input_tokens);
        bucket.completion_tokens = bucket
            .completion_tokens
            .saturating_add(delta_counts.output_tokens);
        bucket.cache_tokens = bucket
            .cache_tokens
            .saturating_add(delta_counts.cache_read());
    }

    let mut rows: Vec<UsageRow> = buckets
        .into_iter()
        .map(|((date, model), bucket)| {
            let cost = estimate_cost(bucket.prompt_tokens, bucket.completion_tokens);
            UsageRow {
                runtime_id: runtime_id.to_owned(),
                date,
                model,
                prompt_tokens: bucket.prompt_tokens,
                completion_tokens: bucket.completion_tokens,
                cache_tokens: bucket.cache_tokens,
                cost_usd: cost,
            }
        })
        .collect();
    rows.sort_by(|a, b| a.date.cmp(&b.date).then(a.model.cmp(&b.model)));
    rows
}

// ─────────────────────────────────────────────────────────────────────────────
// Directory collection (mirrors providers/codex.rs)
// ─────────────────────────────────────────────────────────────────────────────

fn collect_sessions_tree(sessions_dir: &Path) -> Vec<PathBuf> {
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

fn collect_archived_sessions(archived_dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(archived_dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("jsonl"))
        .collect()
}

// ─────────────────────────────────────────────────────────────────────────────
// CodexScanner
// ─────────────────────────────────────────────────────────────────────────────

/// Scans all Codex CLI session files under a Codex home directory and emits one
/// [`UsageRow`] per `(date, model)` pair across all sessions.
///
/// The `runtime_path` passed to [`UsageScanner::scan`] should be the Codex home
/// directory (e.g. `~/.codex`). The scanner walks `sessions/` and
/// `archived_sessions/` and delegates each JSONL file to the per-file reader.
///
/// # Example
///
/// ```no_run
/// use annulus::usage::codex::CodexScanner;
/// use annulus::usage::UsageScanner;
/// use std::path::Path;
///
/// let scanner = CodexScanner;
/// let rows = scanner.scan(Path::new("/home/user/.codex"));
/// println!("found {} rows", rows.len());
/// ```
pub struct CodexScanner;

impl UsageScanner for CodexScanner {
    fn scan(&self, runtime_path: &Path) -> Vec<UsageRow> {
        if !runtime_path.exists() {
            return Vec::new();
        }

        let mut all_paths: Vec<PathBuf> = Vec::new();
        all_paths.extend(collect_sessions_tree(&runtime_path.join("sessions")));
        all_paths.extend(collect_archived_sessions(
            &runtime_path.join("archived_sessions"),
        ));

        let mut all_rows: Vec<UsageRow> = Vec::new();
        for path in &all_paths {
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

    fn write_dated_session(base: &Path, name: &str, content: &str) -> PathBuf {
        let dir = base.join("sessions/2025/09/11");
        fs::create_dir_all(&dir).expect("create sessions dir");
        let path = dir.join(name);
        fs::write(&path, content).expect("write session file");
        path
    }

    fn write_archived_session(base: &Path, name: &str, content: &str) -> PathBuf {
        let dir = base.join("archived_sessions");
        fs::create_dir_all(&dir).expect("create archived dir");
        let path = dir.join(name);
        fs::write(&path, content).expect("write archived file");
        path
    }

    #[test]
    fn codex_scanner_uses_fixture() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let fixture = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/codex/sample-session.jsonl"
        );
        let content = fs::read_to_string(fixture).expect("fixture");
        write_dated_session(dir.path(), "sample-session.jsonl", &content);

        let scanner = CodexScanner;
        let rows = scanner.scan(dir.path());

        assert!(!rows.is_empty(), "should produce rows from fixture");
        // Fixture has two turns for gpt-5 on 2025-09-11.
        let row = rows
            .iter()
            .find(|r| r.model == "gpt-5" && r.date == "2025-09-11")
            .expect("gpt-5 row on 2025-09-11");
        assert_eq!(row.prompt_tokens, 2000, "sum of both turns");
        assert_eq!(row.completion_tokens, 800);
        assert_eq!(row.cache_tokens, 300);
    }

    #[test]
    fn codex_scanner_archived_sessions_included() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let content = "{\"timestamp\":\"2025-09-11T18:25:40.670Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\"model\":\"gpt-5\",\"total_token_usage\":{\"input_tokens\":100,\"cached_input_tokens\":10,\"output_tokens\":50,\"reasoning_output_tokens\":0,\"total_tokens\":150},\"last_token_usage\":{\"input_tokens\":100,\"cached_input_tokens\":10,\"output_tokens\":50,\"reasoning_output_tokens\":0,\"total_tokens\":150}}}}\n";
        write_archived_session(dir.path(), "old-session.jsonl", content);

        let scanner = CodexScanner;
        let rows = scanner.scan(dir.path());
        assert!(!rows.is_empty(), "should scan archived sessions");
    }

    #[test]
    fn codex_scanner_returns_empty_for_missing_dir() {
        let scanner = CodexScanner;
        let rows = scanner.scan(Path::new("/tmp/nonexistent-codex-scanner-annulus"));
        assert!(rows.is_empty(), "missing dir → empty result, no panic");
    }

    #[test]
    fn epoch_secs_to_date_known_values() {
        // 2025-01-01T00:00:00Z = 1735689600
        assert_eq!(epoch_secs_to_date(1_735_689_600), "2025-01-01");
        // 1970-01-01
        assert_eq!(epoch_secs_to_date(0), "1970-01-01");
        // 2026-04-11T00:00:00Z = 1775894400
        assert_eq!(epoch_secs_to_date(1_775_894_400), "2026-04-11");
    }

    #[test]
    fn codex_scanner_date_from_path_hierarchy() {
        // When entry has no timestamp, date should come from path hierarchy.
        let dir = tempfile::TempDir::new().expect("tempdir");
        let content = "{\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\"model\":\"gpt-5\",\"total_token_usage\":{\"input_tokens\":500,\"cached_input_tokens\":0,\"output_tokens\":200,\"reasoning_output_tokens\":0,\"total_tokens\":700},\"last_token_usage\":{\"input_tokens\":500,\"cached_input_tokens\":0,\"output_tokens\":200,\"reasoning_output_tokens\":0,\"total_tokens\":700}}}}\n";
        write_dated_session(dir.path(), "no-ts.jsonl", content);

        let scanner = CodexScanner;
        let rows = scanner.scan(dir.path());
        assert!(!rows.is_empty());
        assert_eq!(rows[0].date, "2025-09-11", "date from path hierarchy");
    }
}
