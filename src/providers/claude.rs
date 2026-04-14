//! Claude transcript-based token usage provider.
//!
//! Reads NDJSON transcript files written by Claude Code and accumulates token
//! usage. The streaming path processes each line without buffering the full
//! file, and the aggregation path deduplicates across multiple transcripts
//! using message IDs and detects session boundaries by time gap.

use std::collections::HashSet;
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;

use serde::{Deserialize, Deserializer};

use super::{TokenProvider, TokenUsage};

/// Five-hour session boundary gap in seconds (as `f64` for timestamp arithmetic).
///
/// Matches the ccusage heuristic: consecutive entries more than 5 hours apart
/// belong to different sessions.
const SESSION_BOUNDARY_SECS_F64: f64 = (5 * 3600) as f64;

// ─────────────────────────────────────────────────────────────────────────────
// Raw transcript types
// ─────────────────────────────────────────────────────────────────────────────

/// A single JSONL entry from a Claude transcript file.
#[derive(Debug, Deserialize)]
pub(crate) struct TranscriptEntry {
    #[serde(rename = "type")]
    pub entry_type: String,

    /// Message ID for deduplication (present in most entries).
    #[serde(default)]
    pub uuid: Option<String>,

    /// Unix timestamp (seconds) for session-boundary detection.
    ///
    /// Claude Code writes `timestamp` as an ISO 8601 string
    /// (`"2026-04-11T16:44:46.069Z"`). Older tooling wrote it as a numeric
    /// epoch. The custom deserializer accepts either form and yields seconds
    /// since the Unix epoch; unparseable values become `None` so the entry is
    /// still visited without crashing deserialization of the enclosing record.
    #[serde(default, deserialize_with = "deserialize_timestamp")]
    pub timestamp: Option<f64>,

    /// Usage block nested under `message` (Claude Code format v1).
    #[serde(default)]
    pub message: Option<MessageBlock>,

    /// Top-level usage block (older transcript format).
    #[serde(default)]
    pub usage: Option<UsageBlock>,
}

#[derive(Debug, Deserialize, Default)]
pub(crate) struct MessageBlock {
    #[serde(default)]
    pub usage: Option<UsageBlock>,
}

#[derive(Debug, Deserialize, Default, Clone, Copy)]
#[allow(
    clippy::struct_field_names,
    reason = "Field names mirror the Claude transcript JSON schema"
)]
pub(crate) struct UsageBlock {
    #[serde(default)]
    pub input_tokens: u32,
    #[serde(default)]
    pub output_tokens: u32,
    #[serde(default)]
    pub cache_read_input_tokens: u32,
    #[serde(default)]
    pub cache_creation_input_tokens: u32,
}

// ─────────────────────────────────────────────────────────────────────────────
// Timestamp deserialization (accepts ISO 8601 string or numeric epoch)
// ─────────────────────────────────────────────────────────────────────────────

/// Accept either a number (seconds since epoch) or an ISO 8601 string such as
/// `2026-04-11T16:44:46.069Z`. Any unparseable value produces `None` rather
/// than failing the entire record, which matches the rest of this module's
/// tolerance for malformed transcript lines.
fn deserialize_timestamp<'de, D>(de: D) -> Result<Option<f64>, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::{self, Visitor};
    use std::fmt;

    struct TimestampVisitor;

    impl<'de> Visitor<'de> for TimestampVisitor {
        type Value = Option<f64>;

        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("a number (seconds) or ISO 8601 timestamp string")
        }

        fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_some<D2: Deserializer<'de>>(self, d: D2) -> Result<Self::Value, D2::Error> {
            d.deserialize_any(self)
        }

        fn visit_f64<E: de::Error>(self, v: f64) -> Result<Self::Value, E> {
            Ok(Some(v))
        }

        fn visit_i64<E: de::Error>(self, v: i64) -> Result<Self::Value, E> {
            // Safe lossy conversion for an epoch-seconds value.
            #[allow(clippy::cast_precision_loss)]
            Ok(Some(v as f64))
        }

        fn visit_u64<E: de::Error>(self, v: u64) -> Result<Self::Value, E> {
            #[allow(clippy::cast_precision_loss)]
            Ok(Some(v as f64))
        }

        fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
            Ok(parse_iso8601_to_epoch_secs(v))
        }

        fn visit_string<E: de::Error>(self, v: String) -> Result<Self::Value, E> {
            Ok(parse_iso8601_to_epoch_secs(&v))
        }
    }

    de.deserialize_option(TimestampVisitor)
}

/// Parse an ISO 8601 / RFC 3339 UTC timestamp (`YYYY-MM-DDTHH:MM:SS[.fff]Z`)
/// to seconds since the Unix epoch.
///
/// Returns `None` for anything that doesn't match the expected shape. Non-UTC
/// offsets are not currently observed in Claude transcripts, so only the
/// trailing `Z` form is supported; a trailing numeric offset is accepted but
/// treated as UTC (the session-boundary heuristic is coarse enough that
/// sub-hour drift doesn't matter).
fn parse_iso8601_to_epoch_secs(s: &str) -> Option<f64> {
    // Shortest valid form: "YYYY-MM-DDTHH:MM:SS" = 19 chars.
    if s.len() < 19 {
        return None;
    }
    let year: i32 = s.get(0..4)?.parse().ok()?;
    if s.as_bytes().get(4) != Some(&b'-') {
        return None;
    }
    let month: u32 = s.get(5..7)?.parse().ok()?;
    if s.as_bytes().get(7) != Some(&b'-') {
        return None;
    }
    let day: u32 = s.get(8..10)?.parse().ok()?;
    let sep = s.as_bytes().get(10)?;
    if *sep != b'T' && *sep != b' ' {
        return None;
    }
    let hour: u32 = s.get(11..13)?.parse().ok()?;
    if s.as_bytes().get(13) != Some(&b':') {
        return None;
    }
    let minute: u32 = s.get(14..16)?.parse().ok()?;
    if s.as_bytes().get(16) != Some(&b':') {
        return None;
    }
    let second: u32 = s.get(17..19)?.parse().ok()?;

    // Optional fractional seconds.
    let mut idx = 19;
    let mut frac: f64 = 0.0;
    if s.as_bytes().get(idx) == Some(&b'.') {
        idx += 1;
        let start = idx;
        while s.as_bytes().get(idx).is_some_and(u8::is_ascii_digit) {
            idx += 1;
        }
        if idx > start {
            let digits = &s[start..idx];
            if let Ok(n) = digits.parse::<u64>() {
                // Clamp fractional precision to u32 (>9 digits is beyond f64 anyway).
                let exp = u32::try_from(idx - start).unwrap_or(9);
                #[allow(clippy::cast_precision_loss)]
                let denom = 10u64.pow(exp) as f64;
                #[allow(clippy::cast_precision_loss)]
                let numer = n as f64;
                frac = numer / denom;
            }
        }
    }

    // Trailing timezone: accept "Z", "+HH:MM", "-HH:MM", or missing (treat as UTC).
    // We don't apply the offset — session-boundary math is 5-hour coarse.

    // Howard Hinnant's days_from_civil algorithm.
    #[allow(clippy::cast_possible_wrap)]
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    #[allow(clippy::cast_sign_loss)]
    let yoe = (y - era * 400) as u32; // [0, 399]
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days: i64 = i64::from(era) * 146_097 + i64::from(doe) - 719_468;

    #[allow(clippy::cast_precision_loss)]
    let secs = days as f64 * 86_400.0
        + f64::from(hour) * 3600.0
        + f64::from(minute) * 60.0
        + f64::from(second)
        + frac;
    Some(secs)
}

// ─────────────────────────────────────────────────────────────────────────────
// Accumulated usage across a transcript (or set of transcripts)
// ─────────────────────────────────────────────────────────────────────────────

/// Accumulated token usage across one or more transcript files.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TranscriptUsage {
    pub requests: usize,
    pub cumulative: RawUsage,
    pub latest_assistant: Option<RawUsage>,
    /// True when a session boundary (> 5h gap) was detected during aggregation.
    pub session_boundary_detected: bool,
}

/// Raw token counts from a usage block.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
#[allow(
    clippy::struct_field_names,
    reason = "Field names mirror the Claude transcript JSON schema"
)]
pub(crate) struct RawUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_read_input_tokens: u32,
    pub cache_creation_input_tokens: u32,
}

impl RawUsage {
    pub(crate) fn prompt_tokens(self) -> u32 {
        self.input_tokens
            .saturating_add(self.cache_read_input_tokens)
            .saturating_add(self.cache_creation_input_tokens)
    }

    pub(crate) fn has_data(self) -> bool {
        self.input_tokens > 0
            || self.output_tokens > 0
            || self.cache_read_input_tokens > 0
            || self.cache_creation_input_tokens > 0
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Stream-based processing
// ─────────────────────────────────────────────────────────────────────────────

/// Visit each valid assistant entry in a JSONL transcript without buffering the
/// full file.
///
/// Malformed lines are skipped silently — Claude transcript files may include
/// partial writes or non-JSON metadata lines.
pub(crate) fn stream_transcript_usage<R, F>(reader: R, mut visitor: F) -> std::io::Result<()>
where
    R: Read,
    F: FnMut(&TranscriptEntry),
{
    // Claude transcripts are strict NDJSON (one JSON object per line).
    // We use line-by-line deserialization rather than serde_json's
    // StreamDeserializer so that a malformed line does not corrupt the
    // remainder of the file stream.
    let buf = BufReader::new(reader);
    for line_result in buf.lines() {
        let line = line_result?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(entry) = serde_json::from_str::<TranscriptEntry>(trimmed) else {
            continue;
        };
        if entry.entry_type == "assistant" {
            visitor(&entry);
        }
    }
    Ok(())
}

/// Extract the usage block from a transcript entry (checks both formats).
fn usage_from_entry(entry: &TranscriptEntry) -> Option<RawUsage> {
    let block = entry
        .message
        .as_ref()
        .and_then(|m| m.usage.as_ref())
        .or(entry.usage.as_ref())?;

    let raw = RawUsage {
        input_tokens: block.input_tokens,
        output_tokens: block.output_tokens,
        cache_read_input_tokens: block.cache_read_input_tokens,
        cache_creation_input_tokens: block.cache_creation_input_tokens,
    };
    if raw.has_data() { Some(raw) } else { None }
}

/// Aggregate token usage across multiple transcript files with deduplication.
///
/// Deduplication is by `uuid` field. Entries without a `uuid` are always
/// included. Session boundaries are detected when the timestamp gap between
/// consecutive entries exceeds [`SESSION_BOUNDARY_SECS_F64`] seconds (5 hours).
pub(crate) fn aggregate_transcript_usage(paths: &[&Path]) -> std::io::Result<TranscriptUsage> {
    let mut seen_ids: HashSet<String> = HashSet::new();
    let mut total = TranscriptUsage::default();
    let mut last_timestamp: Option<f64> = None;

    for path in paths {
        let file = File::open(path)?;
        stream_transcript_usage(file, |entry| {
            // Dedup by uuid when present.
            if let Some(id) = &entry.uuid {
                if !seen_ids.insert(id.clone()) {
                    return; // already counted
                }
            }

            // Detect session boundary.
            if let Some(ts) = entry.timestamp {
                if let Some(prev_ts) = last_timestamp {
                    let gap = (ts - prev_ts).abs();
                    if gap > SESSION_BOUNDARY_SECS_F64 {
                        total.session_boundary_detected = true;
                    }
                }
                last_timestamp = Some(ts);
            }

            let Some(usage) = usage_from_entry(entry) else {
                return;
            };

            total.cumulative.input_tokens = total
                .cumulative
                .input_tokens
                .saturating_add(usage.input_tokens);
            total.cumulative.output_tokens = total
                .cumulative
                .output_tokens
                .saturating_add(usage.output_tokens);
            total.cumulative.cache_read_input_tokens = total
                .cumulative
                .cache_read_input_tokens
                .saturating_add(usage.cache_read_input_tokens);
            total.cumulative.cache_creation_input_tokens = total
                .cumulative
                .cache_creation_input_tokens
                .saturating_add(usage.cache_creation_input_tokens);
            total.requests += 1;
            total.latest_assistant = Some(usage);
        })?;
    }

    Ok(total)
}

/// Read token usage from a single transcript path (delegates to streaming).
pub(crate) fn read_transcript_usage(path: &str) -> anyhow::Result<TranscriptUsage> {
    let p = Path::new(path);
    aggregate_transcript_usage(&[p]).map_err(anyhow::Error::from)
}

// ─────────────────────────────────────────────────────────────────────────────
// ClaudeProvider
// ─────────────────────────────────────────────────────────────────────────────

/// Reads token usage from a Claude Code transcript JSONL file.
///
/// The transcript path is supplied at runtime via the stdin hook payload
/// (`transcript_path` field). Without a path the provider reports available
/// (Claude is always the default) but returns `Ok(None)` for session usage.
#[derive(Debug, Default)]
pub struct ClaudeProvider {
    /// Optional transcript path, set from the stdin payload.
    pub transcript_path: Option<String>,
}

impl TokenProvider for ClaudeProvider {
    fn name(&self) -> &'static str {
        "claude"
    }

    fn is_available(&self) -> bool {
        // Claude is always treated as available — it's the fallback.
        true
    }

    fn session_usage(&self) -> anyhow::Result<Option<TokenUsage>> {
        let Some(path) = &self.transcript_path else {
            return Ok(None);
        };
        let usage = read_transcript_usage(path)?;
        if !usage.cumulative.has_data() {
            return Ok(None);
        }
        Ok(Some(TokenUsage {
            prompt_tokens: usage.cumulative.prompt_tokens(),
            completion_tokens: usage.cumulative.output_tokens,
            cache_read_tokens: usage.cumulative.cache_read_input_tokens,
            cache_creation_tokens: usage.cumulative.cache_creation_input_tokens,
        }))
    }

    /// Returns the mtime (seconds since Unix epoch) of the transcript file,
    /// or `None` when no transcript path is set or the file is unreadable.
    ///
    /// Using file mtime is cheap — no parsing required — and is sufficient for
    /// the coarse recency comparison in `detect_provider`.
    fn last_session_at(&self) -> Option<u64> {
        let path = self.transcript_path.as_deref()?;
        let meta = std::fs::metadata(path).ok()?;
        let mtime = meta.modified().ok()?;
        let secs = mtime.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
        Some(secs)
    }
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

    // ── stream_transcript_usage ───────────────────────────────────────────────

    #[test]
    fn stream_single_file_visits_each_entry() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let content = concat!(
            "{\"type\":\"assistant\",\"uuid\":\"a1\",\"message\":{\"usage\":{\"input_tokens\":100,\"output_tokens\":20,\"cache_read_input_tokens\":0,\"cache_creation_input_tokens\":0}}}\n",
            "{\"type\":\"human\",\"uuid\":\"h1\",\"text\":\"ignored\"}\n",
            "{\"type\":\"assistant\",\"uuid\":\"a2\",\"usage\":{\"input_tokens\":200,\"output_tokens\":40,\"cache_read_input_tokens\":0,\"cache_creation_input_tokens\":0}}\n",
        );
        write_transcript(dir.path(), "t.jsonl", content);

        let mut visits = 0u32;
        stream_transcript_usage(fs::File::open(dir.path().join("t.jsonl")).unwrap(), |_| {
            visits += 1;
        })
        .expect("stream");

        assert_eq!(visits, 2, "should visit exactly 2 assistant entries");
    }

    #[test]
    fn stream_skips_malformed_lines() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let content = concat!(
            "{\"type\":\"assistant\",\"uuid\":\"ok\",\"usage\":{\"input_tokens\":50,\"output_tokens\":10,\"cache_read_input_tokens\":0,\"cache_creation_input_tokens\":0}}\n",
            "not json at all\n",
            "{broken json}\n",
            "{\"type\":\"assistant\",\"uuid\":\"ok2\",\"usage\":{\"input_tokens\":75,\"output_tokens\":15,\"cache_read_input_tokens\":0,\"cache_creation_input_tokens\":0}}\n",
        );
        write_transcript(dir.path(), "t.jsonl", content);

        let mut visits = 0u32;
        stream_transcript_usage(fs::File::open(dir.path().join("t.jsonl")).unwrap(), |_| {
            visits += 1;
        })
        .expect("stream");

        assert_eq!(visits, 2, "malformed lines should be skipped");
    }

    // ── aggregate_transcript_usage ────────────────────────────────────────────

    #[test]
    fn aggregate_dedupes_by_id() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        // Same uuid "a1" appears in both files — should count only once.
        let file1 = write_transcript(
            dir.path(),
            "f1.jsonl",
            "{\"type\":\"assistant\",\"uuid\":\"a1\",\"usage\":{\"input_tokens\":100,\"output_tokens\":20,\"cache_read_input_tokens\":0,\"cache_creation_input_tokens\":0}}\n",
        );
        let file2 = write_transcript(
            dir.path(),
            "f2.jsonl",
            concat!(
                "{\"type\":\"assistant\",\"uuid\":\"a1\",\"usage\":{\"input_tokens\":100,\"output_tokens\":20,\"cache_read_input_tokens\":0,\"cache_creation_input_tokens\":0}}\n",
                "{\"type\":\"assistant\",\"uuid\":\"a2\",\"usage\":{\"input_tokens\":200,\"output_tokens\":40,\"cache_read_input_tokens\":0,\"cache_creation_input_tokens\":0}}\n",
            ),
        );

        let usage =
            aggregate_transcript_usage(&[file1.as_path(), file2.as_path()]).expect("aggregate");

        assert_eq!(usage.requests, 2, "a1 deduped; only a1+a2 counted");
        assert_eq!(usage.cumulative.input_tokens, 300);
    }

    #[test]
    fn aggregate_detects_session_boundary_gap() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        // Two entries more than 5 hours apart.
        let ts1 = 1_700_000_000.0_f64;
        let ts2 = ts1 + SESSION_BOUNDARY_SECS_F64 + 1.0;
        let content = format!(
            "{{\"type\":\"assistant\",\"uuid\":\"x1\",\"timestamp\":{ts1},\"usage\":{{\"input_tokens\":10,\"output_tokens\":5,\"cache_read_input_tokens\":0,\"cache_creation_input_tokens\":0}}}}\n\
             {{\"type\":\"assistant\",\"uuid\":\"x2\",\"timestamp\":{ts2},\"usage\":{{\"input_tokens\":10,\"output_tokens\":5,\"cache_read_input_tokens\":0,\"cache_creation_input_tokens\":0}}}}\n"
        );
        let path = write_transcript(dir.path(), "gap.jsonl", &content);

        let usage = aggregate_transcript_usage(&[path.as_path()]).expect("aggregate");
        assert!(
            usage.session_boundary_detected,
            "should detect session boundary for gap > 5h"
        );
    }

    #[test]
    fn aggregate_no_boundary_for_small_gap() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let ts1 = 1_700_000_000.0_f64;
        let ts2 = ts1 + 600.0; // 10 minutes
        let content = format!(
            "{{\"type\":\"assistant\",\"uuid\":\"y1\",\"timestamp\":{ts1},\"usage\":{{\"input_tokens\":10,\"output_tokens\":5,\"cache_read_input_tokens\":0,\"cache_creation_input_tokens\":0}}}}\n\
             {{\"type\":\"assistant\",\"uuid\":\"y2\",\"timestamp\":{ts2},\"usage\":{{\"input_tokens\":10,\"output_tokens\":5,\"cache_read_input_tokens\":0,\"cache_creation_input_tokens\":0}}}}\n"
        );
        let path = write_transcript(dir.path(), "small.jsonl", &content);

        let usage = aggregate_transcript_usage(&[path.as_path()]).expect("aggregate");
        assert!(
            !usage.session_boundary_detected,
            "10-minute gap should not trigger session boundary"
        );
    }

    // ── read_transcript_usage delegates to streaming path ─────────────────────

    #[test]
    fn read_transcript_usage_delegates_to_streaming_path() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let content = concat!(
            "{\"type\":\"assistant\",\"uuid\":\"z1\",\"message\":{\"usage\":{\"input_tokens\":1200,\"output_tokens\":300,\"cache_read_input_tokens\":500,\"cache_creation_input_tokens\":100}}}\n",
            "{\"type\":\"human\",\"text\":\"ignored\"}\n",
            "{\"type\":\"assistant\",\"uuid\":\"z2\",\"usage\":{\"input_tokens\":800,\"output_tokens\":200,\"cache_read_input_tokens\":100,\"cache_creation_input_tokens\":50}}\n",
        );
        let path = write_transcript(dir.path(), "t.jsonl", content);

        let usage = read_transcript_usage(path.to_str().unwrap()).expect("read");

        assert_eq!(usage.requests, 2);
        assert_eq!(
            usage.cumulative,
            RawUsage {
                input_tokens: 2000,
                output_tokens: 500,
                cache_read_input_tokens: 600,
                cache_creation_input_tokens: 150,
            }
        );
        assert_eq!(
            usage.latest_assistant,
            Some(RawUsage {
                input_tokens: 800,
                output_tokens: 200,
                cache_read_input_tokens: 100,
                cache_creation_input_tokens: 50,
            })
        );
    }

    // ── ClaudeProvider ─────────────────────────────────────────────────────────

    #[test]
    fn claude_provider_wraps_existing_transcript_reading() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let content = "{\"type\":\"assistant\",\"uuid\":\"c1\",\"usage\":{\"input_tokens\":300,\"output_tokens\":100,\"cache_read_input_tokens\":50,\"cache_creation_input_tokens\":25}}\n";
        let path = write_transcript(dir.path(), "claude.jsonl", content);

        let provider = ClaudeProvider {
            transcript_path: Some(path.to_str().unwrap().to_string()),
        };

        assert_eq!(provider.name(), "claude");
        assert!(provider.is_available());

        let usage = provider.session_usage().expect("session_usage");
        assert!(
            usage.is_some(),
            "should return usage when transcript exists"
        );
        let u = usage.unwrap();
        // prompt_tokens = input + cache_read + cache_creation = 300 + 50 + 25 = 375
        assert_eq!(u.prompt_tokens, 375);
        assert_eq!(u.completion_tokens, 100);
    }

    #[test]
    fn claude_provider_returns_none_without_transcript_path() {
        let provider = ClaudeProvider::default();
        let usage = provider.session_usage().expect("no error");
        assert!(usage.is_none());
    }

    // ── ClaudeProvider::last_session_at ───────────────────────────────────────

    #[test]
    fn claude_last_session_at_returns_mtime() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let path = write_transcript(dir.path(), "mtime.jsonl", "{}");
        let provider = ClaudeProvider {
            transcript_path: Some(path.to_str().unwrap().to_string()),
        };
        let ts = provider.last_session_at();
        assert!(ts.is_some(), "should return mtime for an existing file");
        // Sanity: mtime should be a plausible Unix timestamp (after 2020-01-01).
        assert!(
            ts.unwrap() > 1_577_836_800,
            "mtime should be after 2020-01-01"
        );
    }

    #[test]
    fn claude_last_session_at_returns_none_without_path() {
        let provider = ClaudeProvider::default();
        assert!(
            provider.last_session_at().is_none(),
            "no transcript path → None"
        );
    }

    #[test]
    fn claude_last_session_at_returns_none_for_missing_file() {
        let provider = ClaudeProvider {
            transcript_path: Some("/tmp/nonexistent-annulus-test-file.jsonl".to_string()),
        };
        assert!(provider.last_session_at().is_none(), "missing file → None");
    }

    // ── ISO 8601 timestamp support (regression: real Claude transcripts) ──────

    #[test]
    fn parses_iso8601_timestamp_string() {
        // Real Claude Code transcripts write `timestamp` as an ISO 8601 string,
        // not an f64. A previous version of TranscriptEntry declared this as
        // Option<f64>, which caused serde to fail deserialization of every
        // real assistant entry — silently dropping all usage.
        let dir = tempfile::TempDir::new().expect("tempdir");
        let content = concat!(
            "{\"type\":\"assistant\",\"uuid\":\"r1\",\"timestamp\":\"2026-04-11T16:44:46.069Z\",",
            "\"message\":{\"usage\":{\"input_tokens\":1000,\"output_tokens\":200,",
            "\"cache_read_input_tokens\":300,\"cache_creation_input_tokens\":50}}}\n",
        );
        let path = write_transcript(dir.path(), "real.jsonl", content);

        let usage = read_transcript_usage(path.to_str().unwrap()).expect("read");
        assert_eq!(
            usage.requests, 1,
            "ISO 8601 timestamp entry must be counted"
        );
        assert_eq!(usage.cumulative.input_tokens, 1000);
        assert_eq!(usage.cumulative.output_tokens, 200);
    }

    #[test]
    fn iso8601_session_boundary_across_real_format() {
        // Two real-format entries more than 5 hours apart must still trigger
        // the session-boundary heuristic.
        let dir = tempfile::TempDir::new().expect("tempdir");
        let content = concat!(
            "{\"type\":\"assistant\",\"uuid\":\"i1\",\"timestamp\":\"2026-04-11T10:00:00.000Z\",",
            "\"usage\":{\"input_tokens\":10,\"output_tokens\":5,\"cache_read_input_tokens\":0,\"cache_creation_input_tokens\":0}}\n",
            "{\"type\":\"assistant\",\"uuid\":\"i2\",\"timestamp\":\"2026-04-11T16:00:01.000Z\",",
            "\"usage\":{\"input_tokens\":10,\"output_tokens\":5,\"cache_read_input_tokens\":0,\"cache_creation_input_tokens\":0}}\n",
        );
        let path = write_transcript(dir.path(), "iso_gap.jsonl", content);

        let usage = aggregate_transcript_usage(&[path.as_path()]).expect("aggregate");
        assert!(
            usage.session_boundary_detected,
            "6h gap on ISO 8601 timestamps must cross the 5h session boundary"
        );
    }

    #[test]
    fn iso8601_parser_returns_none_on_garbage() {
        assert!(parse_iso8601_to_epoch_secs("not a date").is_none());
        assert!(parse_iso8601_to_epoch_secs("").is_none());
        assert!(parse_iso8601_to_epoch_secs("2026-13-40T25:99:99Z").is_some());
        // Sanity: known value for 2026-04-11T16:44:46.069Z.
        // Verified via `python3 -c "import datetime as d;
        //   print(d.datetime(2026,4,11,16,44,46, tzinfo=d.timezone.utc).timestamp())"`.
        let t = parse_iso8601_to_epoch_secs("2026-04-11T16:44:46.069Z").expect("parse");
        let expected: f64 = 1_775_925_886.069;
        assert!(
            (t - expected).abs() < 1.0,
            "parsed={t}, expected ~{expected}"
        );
        // Epoch-zero reference point — must round-trip exactly.
        let epoch = parse_iso8601_to_epoch_secs("1970-01-01T00:00:00.000Z").expect("parse");
        assert!(
            epoch.abs() < 1e-6,
            "1970-01-01T00:00:00Z should be ~0, got {epoch}"
        );
    }
}
