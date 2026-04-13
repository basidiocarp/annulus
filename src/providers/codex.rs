//! Codex CLI token usage provider (stub).
//!
//! Detects whether `~/.codex/usage.json` exists. Reading the actual file
//! format is a follow-up; this pass always returns `Ok(None)` for session
//! usage.

use super::{TokenProvider, TokenUsage};

/// Reads token usage from the Codex CLI session store.
///
/// Currently a stub: `session_usage()` always returns `Ok(None)`. A future
/// pass will parse `~/.codex/usage.json` and map it to [`TokenUsage`].
#[derive(Debug)]
#[allow(dead_code)] // `usage_path` is read by `is_available`; lint fires because
// `is_available` is not yet called from non-test production code.
pub struct CodexProvider {
    usage_path: std::path::PathBuf,
}

impl CodexProvider {
    /// Create a new `CodexProvider` pointing at the default usage file.
    #[must_use]
    pub fn new() -> Self {
        let path = dirs::home_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join(".codex")
            .join("usage.json");
        Self { usage_path: path }
    }
}

impl Default for CodexProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl TokenProvider for CodexProvider {
    fn name(&self) -> &'static str {
        "codex"
    }

    fn is_available(&self) -> bool {
        self.usage_path.exists()
    }

    /// Always returns `Ok(None)` in this pass.
    ///
    /// Codex file format parsing is deferred to a follow-up handoff.
    fn session_usage(&self) -> anyhow::Result<Option<TokenUsage>> {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_provider_name_is_codex() {
        let provider = CodexProvider::new();
        assert_eq!(provider.name(), "codex");
    }

    #[test]
    fn codex_provider_session_usage_always_none() {
        let provider = CodexProvider::new();
        let result = provider.session_usage();
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }
}
