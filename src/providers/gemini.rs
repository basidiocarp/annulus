//! Gemini CLI token usage provider (stub).
//!
//! Detects whether `~/.gemini-cli/` directory exists. Reading actual session
//! data is a follow-up; this pass always returns `Ok(None)` for session usage.

use super::{TokenProvider, TokenUsage};

/// Reads token usage from the Gemini CLI session store.
///
/// Currently a stub: `session_usage()` always returns `Ok(None)`. A future
/// pass will parse the Gemini CLI state files under `~/.gemini-cli/`.
#[derive(Debug)]
#[allow(dead_code)] // `state_dir` is read by `is_available`; lint fires because
// `is_available` is not yet called from non-test production code.
pub struct GeminiProvider {
    state_dir: std::path::PathBuf,
}

impl GeminiProvider {
    /// Create a new `GeminiProvider` pointing at the default state directory.
    #[must_use]
    pub fn new() -> Self {
        let dir = dirs::home_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join(".gemini-cli");
        Self { state_dir: dir }
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

    fn is_available(&self) -> bool {
        self.state_dir.exists()
    }

    /// Always returns `Ok(None)` in this pass.
    ///
    /// Gemini CLI state file parsing is deferred to a follow-up handoff.
    fn session_usage(&self) -> anyhow::Result<Option<TokenUsage>> {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gemini_provider_name_is_gemini() {
        let provider = GeminiProvider::new();
        assert_eq!(provider.name(), "gemini");
    }

    #[test]
    fn gemini_provider_session_usage_always_none() {
        let provider = GeminiProvider::new();
        let result = provider.session_usage();
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }
}
