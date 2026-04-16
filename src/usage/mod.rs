//! Per-provider usage aggregation layer.
//!
//! Defines [`UsageRow`] (the row shape for chart-ready output) and the
//! [`UsageScanner`] trait that each provider scanner implements. Storage is
//! handled by [`storage`].

pub mod claude;
pub mod codex;
pub mod gemini;
pub mod storage;

use std::path::Path;

use serde::{Deserialize, Serialize};

/// A single per-(runtime, date, model) usage row.
///
/// Rows are keyed by `(runtime_id, date, model)`. The scanner layer produces
/// one row per unique key from a runtime's transcript files; the storage layer
/// deduplicates on read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[must_use]
pub struct UsageRow {
    /// Stable identifier for the agent runtime session (e.g. session UUID or
    /// file stem). Empty string when the transcript carries no identity.
    pub runtime_id: String,

    /// Calendar date (UTC) on which the usage was recorded, formatted as
    /// `YYYY-MM-DD`.
    pub date: String,

    /// Model name as reported by the transcript (e.g. `"claude-opus-4-6"`).
    pub model: String,

    /// Total prompt tokens consumed on this date for this model.
    pub prompt_tokens: u64,

    /// Total completion tokens produced on this date for this model.
    pub completion_tokens: u64,

    /// Total cache-read tokens on this date for this model.
    pub cache_tokens: u64,

    /// Estimated cost in US dollars for this row.
    pub cost_usd: f64,
}

/// A provider that can scan a runtime path and emit [`UsageRow`] values.
pub trait UsageScanner {
    /// Scan `runtime_path` and return all usage rows found.
    ///
    /// Returns an empty `Vec` when the path is absent, unreadable, or carries
    /// no usable token data. Never panics on I/O or parse errors.
    fn scan(&self, runtime_path: &Path) -> Vec<UsageRow>;
}
