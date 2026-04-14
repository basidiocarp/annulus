//! Provider abstraction for reading token usage from coding agents.
//!
//! Each provider knows how to discover and read session data from one agent's
//! local storage. Built-in providers target Claude, Codex, and Gemini CLI.
//! New providers require a Rust impl — this is not a plugin API.

pub mod claude;
pub mod codex;
pub mod gemini;

pub use claude::ClaudeProvider;
pub use codex::CodexProvider;
pub use gemini::GeminiProvider;

/// Token usage from a single coding-agent session.
///
/// Returned by [`TokenProvider::session_usage`]. Fields are `u32` to match
/// the common transcript integer widths; no model in production has >4B tokens
/// per session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[must_use]
#[allow(
    clippy::struct_field_names,
    reason = "Field names are semantic token categories, not redundant postfixes"
)]
pub struct TokenUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub cache_read_tokens: u32,
    pub cache_creation_tokens: u32,
}

/// A coding-agent session data source.
pub trait TokenProvider {
    /// Short name for display and config ("claude", "codex", "gemini").
    fn name(&self) -> &'static str;

    /// Whether this provider's data source is currently available.
    ///
    /// Used by the auto-detect path and by degradation segment probes.
    fn is_available(&self) -> bool;

    /// Read token usage for the current or most recent session.
    ///
    /// Returns `Ok(None)` when the provider is available but has no data for
    /// the current session (e.g. Codex stub before format parsing is wired).
    ///
    /// # Errors
    ///
    /// Returns an error when the underlying data source exists but cannot be
    /// read (e.g. permission denied, I/O failure, or an unrecoverable parse
    /// error). Missing data sources return `Ok(None)` rather than an error.
    fn session_usage(&self) -> anyhow::Result<Option<TokenUsage>>;

    /// Unix timestamp (seconds) of the provider's most recent session activity,
    /// or `None` if the provider cannot determine recency.
    ///
    /// Used by the auto-detect path to prefer the most recently active
    /// provider. Stub providers that return `None` are skipped during recency
    /// comparison and fall through to the Claude default.
    fn last_session_at(&self) -> Option<u64> {
        None
    }
}

/// Select a provider based on an explicit name or auto-detection.
///
/// # Auto-detection
///
/// When no explicit provider is configured, builds the candidate set (Claude,
/// Codex, Gemini), filters to those that are available, and picks the one with
/// the highest `last_session_at` timestamp. Ties resolve in declaration order
/// (Claude wins). If every available provider returns `None` for recency, Claude
/// is returned as the default.
///
/// # Explicit selection
///
/// Passing `Some("claude")`, `Some("codex")`, or `Some("gemini")` returns that
/// provider unconditionally, whether or not its data source is available. This
/// lets operators lock a provider via `statusline.toml` without auto-detect
/// interfering.
#[must_use]
pub fn detect_provider(explicit: Option<&str>) -> Box<dyn TokenProvider> {
    match explicit {
        Some("codex") => Box::new(CodexProvider::new()),
        Some("gemini") => Box::new(GeminiProvider::new()),
        // "claude" or any unrecognised value falls through to explicit Claude.
        Some(_) => Box::new(ClaudeProvider::default()),
        None => detect_by_recency(),
    }
}

/// Auto-detect the most recently active available provider.
///
/// Candidates are evaluated in declaration order so that ties favour Claude.
/// Providers returning `None` from `last_session_at` are skipped; if all
/// return `None`, Claude is the fallback.
fn detect_by_recency() -> Box<dyn TokenProvider> {
    // Build candidates in preference order (Claude first for tie-breaking).
    let candidates: Vec<Box<dyn TokenProvider>> = vec![
        Box::new(ClaudeProvider::default()),
        Box::new(CodexProvider::new()),
        Box::new(GeminiProvider::new()),
    ];

    // Among available providers that report a timestamp, take the most recent.
    // `max_by_key` over an iterator picks the last maximum in case of ties, so
    // we reverse the iteration order: candidates are in preference order
    // (Claude first), and we want Claude to win ties, so we fold manually.
    let mut best_ts: Option<u64> = None;
    let mut best_idx: usize = 0; // index into candidates that holds the winner

    for (i, candidate) in candidates.iter().enumerate() {
        if !candidate.is_available() {
            continue;
        }
        let Some(ts) = candidate.last_session_at() else {
            continue;
        };
        // Strictly greater — equal timestamps keep the earlier (higher-priority) candidate.
        if best_ts.is_none_or(|prev| ts > prev) {
            best_ts = Some(ts);
            best_idx = i;
        }
    }

    // If any candidate had a timestamp, use the winner; otherwise fall back to Claude.
    if best_ts.is_some() {
        // Consume the winner by rebuilding it — we can't move out of a Vec<Box<dyn ...>>.
        match best_idx {
            1 => Box::new(CodexProvider::new()),
            2 => Box::new(GeminiProvider::new()),
            _ => Box::new(ClaudeProvider::default()),
        }
    } else {
        Box::new(ClaudeProvider::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_provider_returns_a_provider_by_default() {
        // Auto-detection is environment-dependent (depends on which tool has
        // the most recent session on the test machine). This test verifies that
        // detect_provider does not panic and returns a valid provider.
        let provider = detect_provider(None);
        let name = provider.name();
        assert!(
            name == "claude" || name == "codex" || name == "gemini",
            "expected a known provider name, got {name}"
        );
    }

    #[test]
    fn detect_provider_returns_claude_for_explicit_claude() {
        let provider = detect_provider(Some("claude"));
        assert_eq!(provider.name(), "claude");
    }

    #[test]
    fn explicit_codex_provider_is_used() {
        let provider = detect_provider(Some("codex"));
        assert_eq!(provider.name(), "codex");
    }

    #[test]
    fn explicit_gemini_provider_is_used() {
        let provider = detect_provider(Some("gemini"));
        assert_eq!(provider.name(), "gemini");
    }

    #[test]
    fn codex_provider_returns_none_when_home_dir_missing() {
        // Use a non-existent home directory to isolate from the real ~/.codex.
        let provider =
            CodexProvider::with_home(std::path::PathBuf::from("/tmp/nonexistent-codex-annulus"));
        let result = provider.session_usage();
        assert!(result.is_ok(), "codex session_usage should not error");
        assert!(
            result.unwrap().is_none(),
            "codex should return None when home dir is missing"
        );
    }

    #[test]
    fn gemini_provider_returns_none_when_dir_missing() {
        // Use a nonexistent directory to guarantee a deterministic None result.
        let provider = GeminiProvider::with_tmp_dir(std::path::PathBuf::from(
            "/tmp/nonexistent-gemini-annulus",
        ));
        let result = provider.session_usage();
        assert!(result.is_ok(), "gemini session_usage should not error");
        assert!(
            result.unwrap().is_none(),
            "gemini should return None when session dir is missing"
        );
    }

    // ── detect_provider recency tests (using mock providers) ─────────────────

    /// A minimal mock provider for testing recency-based detection.
    struct MockProvider {
        name: &'static str,
        available: bool,
        last_session: Option<u64>,
    }

    impl TokenProvider for MockProvider {
        fn name(&self) -> &'static str {
            self.name
        }

        fn is_available(&self) -> bool {
            self.available
        }

        fn session_usage(&self) -> anyhow::Result<Option<TokenUsage>> {
            Ok(None)
        }

        fn last_session_at(&self) -> Option<u64> {
            self.last_session
        }
    }

    #[test]
    fn codex_last_session_at_none_when_home_missing() {
        // Codex last_session_at returns None when the home directory does not exist.
        let codex =
            CodexProvider::with_home(std::path::PathBuf::from("/tmp/nonexistent-codex-annulus"));
        assert!(codex.last_session_at().is_none());
    }

    #[test]
    fn gemini_last_session_at_does_not_panic() {
        // GeminiProvider is now a real reader. last_session_at() is
        // environment-dependent (depends on whether ~/.gemini/tmp/ has files).
        // This test only verifies the method completes without panicking.
        let gemini = GeminiProvider::new();
        let _ = gemini.last_session_at();
    }

    #[test]
    fn detect_provider_does_not_panic_with_no_explicit_provider() {
        // Verifies that auto-detection completes without panicking regardless
        // of the local environment. The winner is environment-dependent.
        let provider = detect_provider(None);
        let name = provider.name();
        assert!(
            name == "claude" || name == "codex" || name == "gemini",
            "expected a known provider, got {name}"
        );
    }

    #[test]
    fn mock_more_recent_wins_over_older() {
        // Verify that the recency comparison logic picks the higher timestamp.
        let older = MockProvider {
            name: "older",
            available: true,
            last_session: Some(1_000),
        };
        let newer = MockProvider {
            name: "newer",
            available: true,
            last_session: Some(2_000),
        };

        // Fold manually (same logic as detect_by_recency).
        let candidates: Vec<&dyn TokenProvider> = vec![&older, &newer];
        let mut best_ts: Option<u64> = None;
        let mut best_name = "";
        for c in &candidates {
            if !c.is_available() {
                continue;
            }
            let Some(ts) = c.last_session_at() else {
                continue;
            };
            if best_ts.is_none_or(|prev| ts > prev) {
                best_ts = Some(ts);
                best_name = c.name();
            }
        }
        assert_eq!(best_name, "newer");
    }

    #[test]
    fn mock_tie_prefers_earlier_declaration_order() {
        // Equal timestamps: the first candidate (higher priority) must win.
        let first = MockProvider {
            name: "first",
            available: true,
            last_session: Some(1_000),
        };
        let second = MockProvider {
            name: "second",
            available: true,
            last_session: Some(1_000),
        };

        let candidates: Vec<&dyn TokenProvider> = vec![&first, &second];
        let mut best_ts: Option<u64> = None;
        let mut best_name = "";
        for c in &candidates {
            if !c.is_available() {
                continue;
            }
            let Some(ts) = c.last_session_at() else {
                continue;
            };
            if best_ts.is_none_or(|prev| ts > prev) {
                best_ts = Some(ts);
                best_name = c.name();
            }
        }
        // Tie: "first" wins because "second" is not strictly greater.
        assert_eq!(best_name, "first");
    }

    #[test]
    fn mock_unavailable_provider_is_skipped() {
        // An unavailable provider must not be selected even if it has a newer timestamp.
        let unavailable = MockProvider {
            name: "unavailable",
            available: false,
            last_session: Some(9_999_999),
        };
        let available = MockProvider {
            name: "available",
            available: true,
            last_session: Some(1_000),
        };

        let candidates: Vec<&dyn TokenProvider> = vec![&unavailable, &available];
        let mut best_ts: Option<u64> = None;
        let mut best_name = "";
        for c in &candidates {
            if !c.is_available() {
                continue;
            }
            let Some(ts) = c.last_session_at() else {
                continue;
            };
            if best_ts.is_none_or(|prev| ts > prev) {
                best_ts = Some(ts);
                best_name = c.name();
            }
        }
        assert_eq!(best_name, "available");
    }
}
