//! Integration tests for session-scoped provider resolution.
//!
//! Verifies that two different Codex session files with different token counts,
//! each addressed by `session_path` via `with_session_file`, return different
//! usage data. This confirms multi-session awareness works end-to-end.

use std::fs;

use annulus::providers::{CodexProvider, GeminiProvider, TokenProvider};

/// Build a Codex NDJSON session line with the given token counts.
fn codex_session_line(input: u32, cached: u32, output: u32) -> String {
    format!(
        concat!(
            "{{\"timestamp\":\"2025-09-11T18:25:40.670Z\",\"type\":\"event_msg\",",
            "\"payload\":{{\"type\":\"token_count\",\"info\":{{\"model\":\"gpt-5\",",
            "\"total_token_usage\":{{\"input_tokens\":{input},\"cached_input_tokens\":{cached},",
            "\"output_tokens\":{output},\"reasoning_output_tokens\":0,\"total_tokens\":{total}}},",
            "\"last_token_usage\":{{\"input_tokens\":{input},\"cached_input_tokens\":{cached},",
            "\"output_tokens\":{output},\"reasoning_output_tokens\":0,\"total_tokens\":{total}}}}}}}}}\n",
        ),
        input = input,
        cached = cached,
        output = output,
        total = input + cached + output,
    )
}

#[test]
fn two_codex_sessions_return_different_usage() {
    let dir = tempfile::TempDir::new().expect("tempdir");

    // Session A: small token counts.
    let session_a_path = dir.path().join("session-a.jsonl");
    fs::write(&session_a_path, codex_session_line(100, 10, 50)).expect("write session A");

    // Session B: larger token counts.
    let session_b_path = dir.path().join("session-b.jsonl");
    fs::write(&session_b_path, codex_session_line(500, 40, 200)).expect("write session B");

    let provider_a = CodexProvider::with_session_file(session_a_path);
    let provider_b = CodexProvider::with_session_file(session_b_path);

    let usage_a = provider_a
        .session_usage()
        .expect("no error")
        .expect("session A should have usage");
    let usage_b = provider_b
        .session_usage()
        .expect("no error")
        .expect("session B should have usage");

    // They must differ.
    assert_ne!(
        usage_a.prompt_tokens, usage_b.prompt_tokens,
        "different session files must return different prompt_tokens"
    );
    assert_ne!(
        usage_a.completion_tokens, usage_b.completion_tokens,
        "different session files must return different completion_tokens"
    );

    // Verify exact values.
    assert_eq!(usage_a.prompt_tokens, 100);
    assert_eq!(usage_a.completion_tokens, 50);
    assert_eq!(usage_a.cache_read_tokens, 10);

    assert_eq!(usage_b.prompt_tokens, 500);
    assert_eq!(usage_b.completion_tokens, 200);
    assert_eq!(usage_b.cache_read_tokens, 40);
}

#[test]
fn two_gemini_sessions_return_different_usage() {
    let dir = tempfile::TempDir::new().expect("tempdir");

    let session_a_content = r#"[
        {"role":"user","parts":[{"text":"hello"}]},
        {"role":"model","parts":[{"text":"world"}],"usageMetadata":{"promptTokenCount":100,"candidatesTokenCount":50,"totalTokenCount":150}}
    ]"#;
    let session_a_path = dir.path().join("session-a.json");
    fs::write(&session_a_path, session_a_content).expect("write session A");

    let session_b_content = r#"[
        {"role":"user","parts":[{"text":"hello"}]},
        {"role":"model","parts":[{"text":"world"}],"usageMetadata":{"promptTokenCount":500,"candidatesTokenCount":200,"totalTokenCount":700}}
    ]"#;
    let session_b_path = dir.path().join("session-b.json");
    fs::write(&session_b_path, session_b_content).expect("write session B");

    let provider_a = GeminiProvider::with_session_file(session_a_path);
    let provider_b = GeminiProvider::with_session_file(session_b_path);

    let usage_a = provider_a
        .session_usage()
        .expect("no error")
        .expect("session A should have usage");
    let usage_b = provider_b
        .session_usage()
        .expect("no error")
        .expect("session B should have usage");

    assert_ne!(
        usage_a.prompt_tokens, usage_b.prompt_tokens,
        "different session files must return different prompt_tokens"
    );
    assert_ne!(
        usage_a.completion_tokens, usage_b.completion_tokens,
        "different session files must return different completion_tokens"
    );

    assert_eq!(usage_a.prompt_tokens, 100);
    assert_eq!(usage_a.completion_tokens, 50);
    assert_eq!(usage_b.prompt_tokens, 500);
    assert_eq!(usage_b.completion_tokens, 200);
}

#[test]
fn session_file_provider_does_not_read_global_most_recent() {
    let dir = tempfile::TempDir::new().expect("tempdir");

    // Set up a "global" Codex home with a session that has large counts.
    let codex_home = dir.path().join("codex-home");
    let sessions_dir = codex_home.join("sessions/2025/09/11");
    fs::create_dir_all(&sessions_dir).expect("create sessions dir");
    fs::write(
        sessions_dir.join("global.jsonl"),
        codex_session_line(9999, 999, 8888),
    )
    .expect("write global session");

    // Set up a targeted session file with small counts.
    let targeted_path = dir.path().join("targeted.jsonl");
    fs::write(&targeted_path, codex_session_line(42, 5, 20)).expect("write targeted session");

    // with_session_file must ignore the global session.
    let provider = CodexProvider::with_session_file(targeted_path);
    let usage = provider
        .session_usage()
        .expect("no error")
        .expect("targeted session should have usage");

    assert_eq!(usage.prompt_tokens, 42, "must read targeted, not global");
    assert_eq!(usage.completion_tokens, 20);
    assert_eq!(usage.cache_read_tokens, 5);
}

#[test]
fn default_codex_provider_backward_compatible() {
    // Verify that the default CodexProvider (no session_file) still works.
    let provider = CodexProvider::new();
    // Environment-dependent: may or may not have data. Just verify no panic.
    let _ = provider.session_usage();
    let _ = provider.last_session_at();
    let _ = provider.is_available();
}

#[test]
fn default_gemini_provider_backward_compatible() {
    let provider = GeminiProvider::new();
    let _ = provider.session_usage();
    let _ = provider.last_session_at();
    let _ = provider.is_available();
}
