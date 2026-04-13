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
#[allow(dead_code)] // Public API — not all callers are wired in this pass.
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
#[allow(dead_code)] // Trait methods are part of the provider API; `is_available`
// and `session_usage` are not yet called from all production
// paths in this pass.
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
    fn session_usage(&self) -> anyhow::Result<Option<TokenUsage>>;
}

/// Select a provider based on an explicit name or auto-detection.
///
/// # Auto-detection (current pass)
///
/// Auto-detect always returns `ClaudeProvider` in this pass because Codex and
/// Gemini providers return `Ok(None)`. When those providers gain real data
/// parsing, the selection should prefer the provider whose data source has the
/// most-recent session activity.
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
        // "claude" or any unrecognised value falls through to Claude.
        Some(_) | None => Box::new(ClaudeProvider::default()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_provider_returns_claude_by_default() {
        let provider = detect_provider(None);
        assert_eq!(provider.name(), "claude");
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
    fn codex_provider_returns_none_when_file_missing() {
        // In a clean test environment ~/.codex/usage.json should not exist.
        let provider = CodexProvider::new();
        let result = provider.session_usage();
        assert!(result.is_ok(), "codex session_usage should not error");
        assert!(
            result.unwrap().is_none(),
            "codex should return None in this pass"
        );
    }

    #[test]
    fn gemini_provider_returns_none_when_dir_missing() {
        let provider = GeminiProvider::new();
        let result = provider.session_usage();
        assert!(result.is_ok(), "gemini session_usage should not error");
        assert!(
            result.unwrap().is_none(),
            "gemini should return None in this pass"
        );
    }
}
