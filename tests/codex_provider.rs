//! Integration tests for `CodexProvider`.
//!
//! Exercises the public `TokenProvider` trait through the real `CodexProvider`
//! implementation using fixture data and temporary directories.

use std::fs;
use std::path::Path;

use annulus::providers::{CodexProvider, TokenProvider};

fn write_dated_session(base: &Path, name: &str, content: &str) {
    let dir = base.join("sessions/2025/09/11");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(name), content).unwrap();
}

fn write_archived_session(base: &Path, name: &str, content: &str) {
    let dir = base.join("archived_sessions");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(name), content).unwrap();
}

#[test]
fn fixture_aggregation_matches_gold_expectations() {
    let dir = tempfile::TempDir::new().unwrap();
    let fixture = include_str!("fixtures/codex/sample-session.jsonl");
    write_dated_session(dir.path(), "sample.jsonl", fixture);

    let provider = CodexProvider::with_home(dir.path().to_path_buf());
    assert!(provider.is_available(), "home dir exists → available");

    let usage = provider
        .session_usage()
        .expect("no error")
        .expect("should have usage from fixture");

    // Gold values from tests/fixtures/codex/README.md:
    // aggregate: prompt_tokens=2000, completion_tokens=800, cache_read_tokens=300,
    // cache_creation_tokens=0.
    assert_eq!(usage.prompt_tokens, 2000);
    assert_eq!(usage.completion_tokens, 800);
    assert_eq!(usage.cache_read_tokens, 300);
    assert_eq!(usage.cache_creation_tokens, 0);
}

#[test]
fn is_available_false_for_missing_home() {
    let provider = CodexProvider::with_home(std::path::PathBuf::from(
        "/tmp/nonexistent-codex-annulus-integration",
    ));
    assert!(!provider.is_available());
}

#[test]
fn session_usage_returns_none_when_no_session_files_exist() {
    let dir = tempfile::TempDir::new().unwrap();
    let provider = CodexProvider::with_home(dir.path().to_path_buf());
    let result = provider.session_usage().expect("no error");
    assert!(result.is_none());
}

#[test]
fn last_session_at_returns_mtime_for_existing_session() {
    let dir = tempfile::TempDir::new().unwrap();
    write_dated_session(dir.path(), "s.jsonl", "{}");
    let provider = CodexProvider::with_home(dir.path().to_path_buf());
    let ts = provider.last_session_at();
    assert!(ts.is_some(), "last_session_at should return mtime");
    assert!(
        ts.unwrap() > 1_577_836_800,
        "mtime should be after 2020-01-01"
    );
}

#[test]
fn reads_archived_session_when_no_dated_sessions_exist() {
    let dir = tempfile::TempDir::new().unwrap();
    let content = concat!(
        "{\"timestamp\":\"2025-09-11T18:25:40.670Z\",\"type\":\"event_msg\",",
        "\"payload\":{\"type\":\"token_count\",\"info\":{\"model\":\"gpt-5\",",
        "\"total_token_usage\":{\"input_tokens\":300,\"cached_input_tokens\":30,",
        "\"output_tokens\":120,\"reasoning_output_tokens\":0,\"total_tokens\":420},",
        "\"last_token_usage\":{\"input_tokens\":300,\"cached_input_tokens\":30,",
        "\"output_tokens\":120,\"reasoning_output_tokens\":0,\"total_tokens\":420}}}}\n",
    );
    write_archived_session(dir.path(), "archived.jsonl", content);

    let provider = CodexProvider::with_home(dir.path().to_path_buf());
    let usage = provider
        .session_usage()
        .expect("no error")
        .expect("archived session should produce usage");
    assert_eq!(usage.prompt_tokens, 300);
    assert_eq!(usage.completion_tokens, 120);
    assert_eq!(usage.cache_read_tokens, 30);
}

#[test]
fn cumulative_only_path_synthesises_correct_deltas() {
    // Regression: when `last_token_usage` is absent, synthesise per-turn deltas
    // by subtracting successive `total_token_usage` readings.
    let dir = tempfile::TempDir::new().unwrap();
    let content = concat!(
        // Turn 1 cumulative: 1200 in, 200 cached, 500 out.
        "{\"timestamp\":\"2025-09-11T18:25:40.670Z\",\"type\":\"event_msg\",",
        "\"payload\":{\"type\":\"token_count\",\"info\":{\"model\":\"gpt-5\",",
        "\"total_token_usage\":{\"input_tokens\":1200,\"cached_input_tokens\":200,",
        "\"output_tokens\":500,\"reasoning_output_tokens\":0,\"total_tokens\":1700}}}}\n",
        // Turn 2 cumulative: 2000 in, 300 cached, 800 out. Delta = 800, 100, 300.
        "{\"timestamp\":\"2025-09-11T18:40:25.910Z\",\"type\":\"event_msg\",",
        "\"payload\":{\"type\":\"token_count\",\"info\":{\"model\":\"gpt-5\",",
        "\"total_token_usage\":{\"input_tokens\":2000,\"cached_input_tokens\":300,",
        "\"output_tokens\":800,\"reasoning_output_tokens\":0,\"total_tokens\":2800}}}}\n",
    );
    write_dated_session(dir.path(), "cumulative.jsonl", content);

    let provider = CodexProvider::with_home(dir.path().to_path_buf());
    let usage = provider
        .session_usage()
        .expect("no error")
        .expect("cumulative-only file should produce usage");
    assert_eq!(usage.prompt_tokens, 2000);
    assert_eq!(usage.completion_tokens, 800);
    assert_eq!(usage.cache_read_tokens, 300);
}
