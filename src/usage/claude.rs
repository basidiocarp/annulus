//! Claude usage scanner.
//!
//! Reads NDJSON transcript files under a Claude runtime path and produces one
//! [`UsageRow`] per `(date, model)` pair. Reuses the streaming parser from
//! [`crate::providers::claude`] rather than reimplementing the transcript
//! format.

use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;

use super::{UsageRow, UsageScanner};
use crate::providers::claude::TranscriptEntry;

// ─────────────────────────────────────────────────────────────────────────────
// Cost estimation
// ─────────────────────────────────────────────────────────────────────────────

/// Derive a rough USD cost from token counts.
///
/// Uses a per-million-token rate of $3.00 for prompt tokens and $15.00 for
/// completion tokens — approximate midpoint for Claude 3.x/4.x models. The
/// storage layer accumulates the result; callers that need exact figures should
/// recompute from the token fields directly.
fn estimate_cost(prompt_tokens: u64, completion_tokens: u64) -> f64 {
    const PROMPT_RATE: f64 = 3.0 / 1_000_000.0;
    const COMPLETION_RATE: f64 = 15.0 / 1_000_000.0;
    #[allow(clippy::cast_precision_loss)]
    let p = prompt_tokens as f64;
    #[allow(clippy::cast_precision_loss)]
    let c = completion_tokens as f64;
    p * PROMPT_RATE + c * COMPLETION_RATE
}

// ─────────────────────────────────────────────────────────────────────────────
// Date extraction from ISO 8601 timestamp
// ─────────────────────────────────────────────────────────────────────────────

/// Extract the `YYYY-MM-DD` date prefix from an ISO 8601 string such as
/// `"2026-04-11T16:44:46.069Z"`. Returns `None` for anything shorter.
fn date_from_iso8601(ts: &str) -> Option<String> {
    if ts.len() >= 10 {
        Some(ts[..10].to_owned())
    } else {
        None
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Extended transcript entry (adds model + timestamp string)
// ─────────────────────────────────────────────────────────────────────────────

/// Extract (`date_str`, model) from a transcript entry.
///
/// - `date_str` comes from the `timestamp` string field (first 10 chars).
/// - `model` comes from `message.model` when present.
///
/// Returns `None` for entries that carry no usable model or date data.
fn date_and_model(entry: &TranscriptEntry, raw_line: &str) -> Option<(String, String)> {
    // Extract raw timestamp string for the date (the numeric f64 in TranscriptEntry
    // has already lost the string form, so we re-parse from the raw JSON line).
    let date = extract_date_from_raw(raw_line)?;

    // Extract model from message.model field in the raw JSON.
    let model = extract_model_from_raw(raw_line).unwrap_or_else(|| "claude".to_owned());

    // Confirm that the entry actually has token data worth recording.
    if entry
        .message
        .as_ref()
        .and_then(|m| m.usage.as_ref())
        .is_none()
        && entry.usage.is_none()
    {
        return None;
    }

    Some((date, model))
}

fn extract_date_from_raw(line: &str) -> Option<String> {
    // Fast path: find `"timestamp":"` or `"timestamp": "` and grab the date.
    let key = "\"timestamp\"";
    let pos = line.find(key)?;
    let after = &line[pos + key.len()..];
    let after = after.trim_start_matches([' ', ':']);
    let after = after.trim_start_matches('"');
    date_from_iso8601(after)
}

fn extract_model_from_raw(line: &str) -> Option<String> {
    // Look for `"model":"<value>"` inside the JSON line.
    let key = "\"model\"";
    let pos = line.find(key)?;
    let after = line[pos + key.len()..].trim_start();
    let after = after.trim_start_matches(':').trim_start();
    let after = after.trim_start_matches('"');
    let end = after.find('"')?;
    let model = &after[..end];
    if model.is_empty() {
        None
    } else {
        Some(model.to_owned())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Accumulator
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

// ─────────────────────────────────────────────────────────────────────────────
// ClaudeScanner
// ─────────────────────────────────────────────────────────────────────────────

/// Scans a Claude transcript file (NDJSON) and emits one [`UsageRow`] per
/// `(date, model)` pair.
///
/// The `runtime_path` passed to [`UsageScanner::scan`] must point directly at
/// a `.jsonl` transcript file. The scanner reuses the streaming parser from
/// [`crate::providers::claude`] to avoid re-implementing the entry format.
///
/// # Example
///
/// ```no_run
/// use annulus::usage::claude::ClaudeScanner;
/// use annulus::usage::UsageScanner;
/// use std::path::Path;
///
/// let scanner = ClaudeScanner;
/// let rows = scanner.scan(Path::new("/path/to/transcript.jsonl"));
/// println!("found {} rows", rows.len());
/// ```
pub struct ClaudeScanner;

impl UsageScanner for ClaudeScanner {
    fn scan(&self, runtime_path: &Path) -> Vec<UsageRow> {
        scan_transcript_file(runtime_path, "")
    }
}

/// Scan a single transcript file, using `runtime_id` to label rows.
///
/// Exposed for internal reuse (e.g. batch scanning across all project
/// transcripts). When `runtime_id` is empty, the file stem is used.
pub(crate) fn scan_transcript_file(path: &Path, runtime_id: &str) -> Vec<UsageRow> {
    let rid = if runtime_id.is_empty() {
        path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_owned()
    } else {
        runtime_id.to_owned()
    };

    let Ok(file) = fs::File::open(path) else {
        return Vec::new();
    };

    // We need raw line access AND the parsed entry simultaneously, so we read
    // lines ourselves rather than delegating fully to stream_transcript_usage.
    let reader = BufReader::new(file);

    let mut buckets: HashMap<(String, String), Bucket> = HashMap::new();

    for line_result in reader.lines() {
        let Ok(line) = line_result else { continue };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        // Use the existing parser for entry-type filtering and usage extraction.
        let Ok(entry) = serde_json::from_str::<TranscriptEntry>(trimmed) else {
            continue;
        };
        if entry.entry_type != "assistant" {
            continue;
        }

        let Some((date, model)) = date_and_model(&entry, trimmed) else {
            continue;
        };

        let usage_block = entry
            .message
            .as_ref()
            .and_then(|m| m.usage.as_ref())
            .or(entry.usage.as_ref());

        let Some(ub) = usage_block else { continue };

        let bucket = buckets.entry((date, model)).or_default();
        bucket.prompt_tokens = bucket
            .prompt_tokens
            .saturating_add(u64::from(ub.input_tokens));
        bucket.completion_tokens = bucket
            .completion_tokens
            .saturating_add(u64::from(ub.output_tokens));
        bucket.cache_tokens = bucket.cache_tokens.saturating_add(u64::from(
            ub.cache_read_input_tokens
                .saturating_add(ub.cache_creation_input_tokens),
        ));
    }

    let mut rows: Vec<UsageRow> = buckets
        .into_iter()
        .map(|((date, model), bucket)| {
            let cost = estimate_cost(bucket.prompt_tokens, bucket.completion_tokens);
            UsageRow {
                runtime_id: rid.clone(),
                date,
                model,
                prompt_tokens: bucket.prompt_tokens,
                completion_tokens: bucket.completion_tokens,
                cache_tokens: bucket.cache_tokens,
                cost_usd: cost,
            }
        })
        .collect();

    // Stable sort by date then model for deterministic output.
    rows.sort_by(|a, b| a.date.cmp(&b.date).then(a.model.cmp(&b.model)));
    rows
}

/// Scan all `.jsonl` transcript files found inside a Claude projects directory.
///
/// Walks one level of `projects_dir`, collecting all `*.jsonl` files, and
/// delegates each file to [`scan_transcript_file`]. The `runtime_id` is the
/// file stem (UUID) of each transcript.
#[must_use]
pub fn scan_claude_projects(projects_dir: &Path) -> Vec<UsageRow> {
    let Ok(entries) = fs::read_dir(projects_dir) else {
        return Vec::new();
    };

    let mut all: Vec<UsageRow> = Vec::new();
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_file() && p.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            all.extend(scan_transcript_file(&p, ""));
        } else if p.is_dir() {
            // One level deeper (project subdirectory).
            if let Ok(sub_entries) = fs::read_dir(&p) {
                for sub in sub_entries.flatten() {
                    let sp = sub.path();
                    if sp.is_file() && sp.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                        all.extend(scan_transcript_file(&sp, ""));
                    }
                }
            }
        }
    }
    all
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn write_transcript(dir: &Path, name: &str, content: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        fs::write(&path, content).expect("write transcript");
        path
    }

    #[test]
    fn claude_scanner_single_entry_produces_one_row() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let content = concat!(
            "{\"type\":\"assistant\",\"uuid\":\"r1\",\"timestamp\":\"2026-04-11T16:44:46.069Z\",",
            "\"message\":{\"model\":\"claude-opus-4-6\",\"usage\":{\"input_tokens\":1000,\"output_tokens\":200,",
            "\"cache_read_input_tokens\":300,\"cache_creation_input_tokens\":50}}}\n",
        );
        let path = write_transcript(dir.path(), "test-session.jsonl", content);

        let scanner = ClaudeScanner;
        let rows = scanner.scan(&path);

        assert_eq!(rows.len(), 1, "one (date, model) pair → one row");
        let row = &rows[0];
        assert_eq!(row.date, "2026-04-11");
        assert_eq!(row.model, "claude-opus-4-6");
        assert_eq!(row.prompt_tokens, 1000);
        assert_eq!(row.completion_tokens, 200);
        assert_eq!(row.cache_tokens, 350); // 300 + 50
        assert_eq!(row.runtime_id, "test-session");
        assert!(row.cost_usd > 0.0, "cost should be non-zero");
    }

    #[test]
    fn claude_scanner_groups_by_date_and_model() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let content = concat!(
            // Day 1, model A
            "{\"type\":\"assistant\",\"uuid\":\"a1\",\"timestamp\":\"2026-04-10T10:00:00.000Z\",",
            "\"message\":{\"model\":\"claude-opus-4-6\",\"usage\":{\"input_tokens\":100,\"output_tokens\":20,",
            "\"cache_read_input_tokens\":0,\"cache_creation_input_tokens\":0}}}\n",
            // Day 1, model A again (should merge)
            "{\"type\":\"assistant\",\"uuid\":\"a2\",\"timestamp\":\"2026-04-10T11:00:00.000Z\",",
            "\"message\":{\"model\":\"claude-opus-4-6\",\"usage\":{\"input_tokens\":200,\"output_tokens\":40,",
            "\"cache_read_input_tokens\":0,\"cache_creation_input_tokens\":0}}}\n",
            // Day 2, model A
            "{\"type\":\"assistant\",\"uuid\":\"a3\",\"timestamp\":\"2026-04-11T10:00:00.000Z\",",
            "\"message\":{\"model\":\"claude-opus-4-6\",\"usage\":{\"input_tokens\":50,\"output_tokens\":10,",
            "\"cache_read_input_tokens\":0,\"cache_creation_input_tokens\":0}}}\n",
        );
        let path = write_transcript(dir.path(), "multi.jsonl", content);

        let scanner = ClaudeScanner;
        let rows = scanner.scan(&path);

        assert_eq!(rows.len(), 2, "two distinct dates → two rows");
        let day1 = rows.iter().find(|r| r.date == "2026-04-10").expect("day 1");
        assert_eq!(day1.prompt_tokens, 300);
        assert_eq!(day1.completion_tokens, 60);
        let day2 = rows.iter().find(|r| r.date == "2026-04-11").expect("day 2");
        assert_eq!(day2.prompt_tokens, 50);
    }

    #[test]
    fn claude_scanner_skips_entries_without_usage() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let content = concat!(
            "{\"type\":\"human\",\"uuid\":\"h1\",\"timestamp\":\"2026-04-11T10:00:00Z\",\"text\":\"hello\"}\n",
            "{\"type\":\"assistant\",\"uuid\":\"a1\",\"timestamp\":\"2026-04-11T10:00:01Z\",",
            "\"message\":{\"model\":\"claude-opus-4-6\",\"usage\":{\"input_tokens\":10,\"output_tokens\":5,",
            "\"cache_read_input_tokens\":0,\"cache_creation_input_tokens\":0}}}\n",
        );
        let path = write_transcript(dir.path(), "mixed.jsonl", content);

        let scanner = ClaudeScanner;
        let rows = scanner.scan(&path);
        assert_eq!(
            rows.len(),
            1,
            "only the assistant entry should produce a row"
        );
    }

    #[test]
    fn claude_scanner_returns_empty_for_missing_file() {
        let scanner = ClaudeScanner;
        let rows = scanner.scan(Path::new("/tmp/nonexistent-claude-scanner-annulus.jsonl"));
        assert!(rows.is_empty(), "missing file → empty result, no panic");
    }

    #[test]
    fn claude_scanner_falls_back_to_claude_model_when_absent() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        // No "model" field in message block.
        let content = concat!(
            "{\"type\":\"assistant\",\"uuid\":\"x1\",\"timestamp\":\"2026-04-11T10:00:00Z\",",
            "\"message\":{\"usage\":{\"input_tokens\":5,\"output_tokens\":2,",
            "\"cache_read_input_tokens\":0,\"cache_creation_input_tokens\":0}}}\n",
        );
        let path = write_transcript(dir.path(), "no-model.jsonl", content);

        let scanner = ClaudeScanner;
        let rows = scanner.scan(&path);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].model, "claude", "should fall back to 'claude'");
    }

    // Confirms that stream_transcript_usage from the provider module is still
    // reachable from the scanner module, establishing the reuse relationship.
    #[test]
    fn stream_transcript_usage_still_reachable_from_scanner_module() {
        use crate::providers::claude::stream_transcript_usage;
        let content = b"{\"type\":\"assistant\",\"uuid\":\"q1\",\"usage\":{\"input_tokens\":10,\"output_tokens\":5,\"cache_read_input_tokens\":0,\"cache_creation_input_tokens\":0}}\n";
        let mut count = 0u32;
        stream_transcript_usage(content.as_ref(), |_| count += 1).expect("stream");
        assert_eq!(count, 1);
    }
}
