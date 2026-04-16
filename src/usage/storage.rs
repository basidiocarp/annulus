//! Append-only JSONL storage for [`UsageRow`] values.
//!
//! Rows are stored as newline-delimited JSON. Each call to [`append`] writes
//! one line per row, atomically appending to the file. [`read_all`] reads every
//! stored line and deduplicates rows with identical `(runtime_id, date, model)`
//! keys, keeping the last occurrence of each key.
//!
//! This module does not depend on `SQLite` so that the storage path is always
//! available even on targets where the `SQLite` feature is unavailable.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

use super::UsageRow;

// ─────────────────────────────────────────────────────────────────────────────
// Public API
// ─────────────────────────────────────────────────────────────────────────────

/// Append `rows` to the JSONL file at `path`.
///
/// Creates the file (and all parent directories) if it does not exist. Each
/// row is serialised as a single JSON line followed by a newline. Returns
/// immediately when `rows` is empty.
///
/// # Errors
///
/// Returns an error when the file cannot be opened for writing or when
/// serialisation fails.
pub fn append(path: &Path, rows: &[UsageRow]) -> anyhow::Result<()> {
    if rows.is_empty() {
        return Ok(());
    }

    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| anyhow::anyhow!("create storage dir {}: {e}", parent.display()))?;
        }
    }

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| anyhow::anyhow!("open storage file {}: {e}", path.display()))?;

    for row in rows {
        let line =
            serde_json::to_string(row).map_err(|e| anyhow::anyhow!("serialise UsageRow: {e}"))?;
        writeln!(file, "{line}")
            .map_err(|e| anyhow::anyhow!("write to {}: {e}", path.display()))?;
    }

    file.flush()
        .map_err(|e| anyhow::anyhow!("flush {}: {e}", path.display()))?;

    Ok(())
}

/// Read all rows from the JSONL file at `path`, deduplicating by
/// `(runtime_id, date, model)`.
///
/// Returns an empty `Vec` when the file does not exist. Malformed lines are
/// skipped silently. When duplicate keys are present the last occurrence wins
/// (later appends override earlier ones).
///
/// # Errors
///
/// Returns an error only for I/O failures other than file-not-found.
pub fn read_all(path: &Path) -> anyhow::Result<Vec<UsageRow>> {
    if !path.exists() {
        return Ok(Vec::new());
    }

    let file = File::open(path)
        .map_err(|e| anyhow::anyhow!("open storage file {}: {e}", path.display()))?;
    let reader = BufReader::new(file);

    // Use an insertion-ordered map by inserting into a Vec + HashMap index.
    // The HashMap records the Vec position; on duplicate, we overwrite in place.
    let mut index: HashMap<(String, String, String), usize> = HashMap::new();
    let mut rows: Vec<UsageRow> = Vec::new();

    for line_result in reader.lines() {
        let line = line_result.map_err(|e| anyhow::anyhow!("read line: {e}"))?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(row) = serde_json::from_str::<UsageRow>(trimmed) else {
            continue;
        };
        let key = (row.runtime_id.clone(), row.date.clone(), row.model.clone());
        if let Some(&pos) = index.get(&key) {
            // Overwrite the earlier occurrence.
            rows[pos] = row;
        } else {
            index.insert(key, rows.len());
            rows.push(row);
        }
    }

    Ok(rows)
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_row(
        runtime_id: &str,
        date: &str,
        model: &str,
        prompt: u64,
        completion: u64,
    ) -> UsageRow {
        UsageRow {
            runtime_id: runtime_id.to_owned(),
            date: date.to_owned(),
            model: model.to_owned(),
            prompt_tokens: prompt,
            completion_tokens: completion,
            cache_tokens: 0,
            cost_usd: 0.001,
        }
    }

    #[test]
    fn round_trip_single_row() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let path = dir.path().join("usage.jsonl");

        let row = make_row("session-1", "2026-04-11", "claude-opus-4-6", 1000, 200);
        append(&path, std::slice::from_ref(&row)).expect("append");

        let rows = read_all(&path).expect("read_all");
        assert_eq!(rows.len(), 1, "one row stored and retrieved");
        assert_eq!(rows[0].runtime_id, "session-1");
        assert_eq!(rows[0].date, "2026-04-11");
        assert_eq!(rows[0].model, "claude-opus-4-6");
        assert_eq!(rows[0].prompt_tokens, 1000);
        assert_eq!(rows[0].completion_tokens, 200);
    }

    #[test]
    fn round_trip_multiple_rows() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let path = dir.path().join("usage.jsonl");

        let rows = vec![
            make_row("s1", "2026-04-10", "gpt-5", 500, 100),
            make_row("s2", "2026-04-11", "gemini", 300, 80),
        ];
        append(&path, &rows).expect("append");

        let back = read_all(&path).expect("read_all");
        assert_eq!(back.len(), 2);
    }

    #[test]
    fn deduplication_on_read() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let path = dir.path().join("usage.jsonl");

        let first = make_row("s1", "2026-04-11", "claude-opus-4-6", 1000, 200);
        let updated = make_row("s1", "2026-04-11", "claude-opus-4-6", 1500, 300);
        let other = make_row("s2", "2026-04-11", "claude-opus-4-6", 999, 99);

        append(&path, &[first]).expect("append first");
        append(&path, &[updated]).expect("append updated");
        append(&path, &[other]).expect("append other");

        let rows = read_all(&path).expect("read_all");
        assert_eq!(rows.len(), 2, "duplicate key deduplicated to 2 unique rows");

        let s1_row = rows.iter().find(|r| r.runtime_id == "s1").expect("s1");
        assert_eq!(s1_row.prompt_tokens, 1500, "later write wins deduplication");
        let s2_row = rows.iter().find(|r| r.runtime_id == "s2").expect("s2");
        assert_eq!(s2_row.prompt_tokens, 999);
    }

    #[test]
    fn read_all_returns_empty_for_missing_file() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let path = dir.path().join("nonexistent.jsonl");
        let rows = read_all(&path).expect("no error");
        assert!(rows.is_empty(), "missing file → empty Vec");
    }

    #[test]
    fn append_creates_parent_dirs() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let path = dir.path().join("a").join("b").join("usage.jsonl");
        let row = make_row("s1", "2026-04-11", "gemini", 10, 5);
        append(&path, &[row]).expect("append with nested dirs");
        assert!(path.exists(), "file should be created");
    }

    #[test]
    fn append_empty_rows_is_noop() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let path = dir.path().join("usage.jsonl");
        append(&path, &[]).expect("no error");
        assert!(
            !path.exists(),
            "file should not be created for empty append"
        );
    }

    #[test]
    fn malformed_lines_skipped_on_read() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let path = dir.path().join("usage.jsonl");

        // Manually write a file with one good and one bad line.
        let mut f = File::create(&path).expect("create");
        writeln!(f, "{{not valid json}}").expect("write bad line");
        let good = make_row("s1", "2026-04-11", "claude", 100, 20);
        let good_line = serde_json::to_string(&good).expect("serialise");
        writeln!(f, "{good_line}").expect("write good line");
        drop(f);

        let rows = read_all(&path).expect("read_all");
        assert_eq!(rows.len(), 1, "malformed line skipped; good row present");
    }
}
