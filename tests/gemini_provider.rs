//! Integration tests for `GeminiProvider`.
//!
//! Exercises the public `TokenProvider` trait implementation against the
//! fixture in `tests/fixtures/gemini/`. Tests run against the real fixture
//! format documented by handoff #119a.

use std::fs;
use std::path::Path;

use annulus::providers::{GeminiProvider, TokenProvider};

/// Write a `.json` session file at `dir/<name>.json`.
fn write_session(dir: &Path, name: &str, content: &str) -> std::path::PathBuf {
    fs::create_dir_all(dir).expect("create session dir");
    let path = dir.join(name);
    fs::write(&path, content).expect("write session file");
    path
}

#[test]
fn gemini_provider_name_is_gemini() {
    let provider = GeminiProvider::new();
    assert_eq!(provider.name(), "gemini");
}

#[test]
fn gemini_provider_is_unavailable_when_dir_missing() {
    let provider = GeminiProvider::with_tmp_dir(std::path::PathBuf::from(
        "/tmp/nonexistent-gemini-annulus-integration",
    ));
    assert!(!provider.is_available());
}

#[test]
fn gemini_provider_is_available_when_dir_exists() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let provider = GeminiProvider::with_tmp_dir(dir.path().to_path_buf());
    assert!(provider.is_available());
}

#[test]
fn gemini_provider_session_usage_returns_none_without_session_files() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let provider = GeminiProvider::with_tmp_dir(dir.path().to_path_buf());
    let result = provider.session_usage().expect("no error");
    assert!(result.is_none());
}

#[test]
fn gemini_provider_aggregates_fixture_correctly() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let fixture_path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/gemini/sample-session.json"
    );
    let content = fs::read_to_string(fixture_path).expect("fixture readable");
    write_session(dir.path(), "sample-session.json", &content);

    let provider = GeminiProvider::with_tmp_dir(dir.path().to_path_buf());
    let usage = provider
        .session_usage()
        .expect("no I/O error")
        .expect("should have usage data");

    // Gold expectations from tests/fixtures/gemini/README.md:
    // - prompt_tokens: 1534 (last model turn's promptTokenCount; turns 1/2 have
    //   312/847, turn 3 is skipped — no usageMetadata, turn 4 has 1534)
    // - completion_tokens: 87 + 203 + 411 = 701 (turn 3 skipped)
    // - cache_read_tokens: 0 (not reported by Gemini CLI)
    // - cache_creation_tokens: 0 (not reported by Gemini CLI)
    assert_eq!(usage.prompt_tokens, 1534);
    assert_eq!(usage.completion_tokens, 701);
    assert_eq!(usage.cache_read_tokens, 0);
    assert_eq!(usage.cache_creation_tokens, 0);
}

#[test]
fn gemini_provider_last_session_at_returns_none_without_files() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let provider = GeminiProvider::with_tmp_dir(dir.path().to_path_buf());
    assert!(provider.last_session_at().is_none());
}

#[test]
fn gemini_provider_last_session_at_returns_mtime() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    write_session(dir.path(), "session.json", "[]");
    let provider = GeminiProvider::with_tmp_dir(dir.path().to_path_buf());
    let ts = provider.last_session_at();
    assert!(ts.is_some(), "mtime must be present when file exists");
    assert!(
        ts.expect("ts is Some") > 1_577_836_800,
        "mtime must be after 2020-01-01"
    );
}

#[test]
fn gemini_provider_tolerates_malformed_json() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    write_session(dir.path(), "truncated.json", "[{\"role\":\"user\",");
    let provider = GeminiProvider::with_tmp_dir(dir.path().to_path_buf());
    let result = provider.session_usage().expect("no error returned");
    assert!(
        result.is_none(),
        "malformed JSON must yield None, not an error"
    );
}

#[test]
fn gemini_provider_tolerates_empty_array() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    write_session(dir.path(), "empty.json", "[]");
    let provider = GeminiProvider::with_tmp_dir(dir.path().to_path_buf());
    let result = provider.session_usage().expect("no error");
    assert!(result.is_none());
}
