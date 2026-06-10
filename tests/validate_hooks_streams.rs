//! Integration tests for `annulus validate-hooks` stream splitting and exit codes.
//!
//! Verifies the behavioral contract: stdout carries only resolved hook paths,
//! stderr carries all human-readable prose (labels, summaries), and the process
//! exits 1 when any hook is stale or broken. This contract is depended on by
//! external scripts that parse stdout.
//!
//! Unix-only: the harness controls which `settings.json` the binary reads by
//! overriding `$HOME`, but `dirs::home_dir()` consults the profile known-folder
//! API on Windows and ignores `$HOME`, so the fixture cannot be deterministically
//! targeted there. Windows integration coverage is tracked as a follow-up
//! (`.handoffs/proposals/annulus/validate-hooks-broken-and-windows-coverage.md`).
#![cfg(unix)]

use std::fs;
use std::path::PathBuf;
use std::process::Command;

/// Fixture: a minimal valid settings.json with a hook registered to a specific path.
fn settings_json_with_hook(hook_path: &str) -> String {
    format!(
        r#"{{
  "hooks": {{
    "PreToolUse": [
      {{
        "matcher": "Bash",
        "hooks": [
          {{
            "type": "command",
            "command": "{hook_path}"
          }}
        ]
      }}
    ]
  }}
}}"#
    )
}

/// Returns the path to the built annulus binary.
fn annulus_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_annulus"))
}

/// Test: a stale (non-existent) hook path appears on stdout, [STALE] label
/// appears on stderr, and the process exits 1.
#[test]
fn test_stale_hook_exits_1_path_on_stdout_label_on_stderr() {
    let home_tmp = tempfile::TempDir::new().expect("create home tempdir");
    let work_tmp = tempfile::TempDir::new().expect("create work tempdir");

    let home_claude_dir = home_tmp.path().join(".claude");
    fs::create_dir_all(&home_claude_dir).expect("create .claude dir");

    let stale_hook_path = home_tmp.path().join("hooks").join("nonexistent.sh");
    let settings_path = home_claude_dir.join("settings.json");

    let hook_abs_path = stale_hook_path.to_string_lossy().to_string();
    fs::write(&settings_path, settings_json_with_hook(&hook_abs_path))
        .expect("write settings.json");

    // Spawn with HOME pointing to home_tmp and cwd pointing to an empty work_tmp,
    // so find_settings_files() scans HOME scope and project scope (which is empty).
    let output = Command::new(annulus_bin())
        .arg("validate-hooks")
        .env("HOME", home_tmp.path())
        .current_dir(work_tmp.path())
        .output()
        .expect("run annulus validate-hooks");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    // Verify exit code is exactly 1 (not merely non-zero) due to stale hook.
    assert_eq!(
        output.status.code(),
        Some(1),
        "expected exit code exactly 1 for stale hook, got: {:?}",
        output.status.code()
    );

    // Verify the stale hook path appears on stdout.
    assert!(
        stdout.contains(hook_abs_path.as_str()),
        "expected stdout to contain stale hook path '{hook_abs_path}', got: '{stdout}'"
    );

    // Verify [STALE] label appears on stderr.
    assert!(
        stderr.contains("[STALE]"),
        "expected stderr to contain '[STALE]' label, got: '{stderr}'"
    );

    // Verify no prose (like [STALE], [OK], or summary) appears on stdout.
    // Stdout should be exactly the path line (one line per hook).
    let stdout_lines: Vec<&str> = stdout.trim().lines().collect();
    assert_eq!(
        stdout_lines.len(),
        1,
        "expected exactly 1 line on stdout (the hook path), got: {stdout_lines:?}"
    );
    assert_eq!(
        stdout_lines[0], hook_abs_path,
        "expected stdout to be exactly the hook path"
    );
}

/// Test: an existing, executable .sh hook path appears on stdout, [OK] label
/// appears on stderr, and the process exits 0.
#[test]
#[cfg(unix)]
fn test_ok_hook_exits_0_path_on_stdout_label_on_stderr() {
    use std::os::unix::fs::PermissionsExt;

    let home_tmp = tempfile::TempDir::new().expect("create home tempdir");
    let work_tmp = tempfile::TempDir::new().expect("create work tempdir");

    let home_claude_dir = home_tmp.path().join(".claude");
    fs::create_dir_all(&home_claude_dir).expect("create .claude dir");

    let hooks_dir = home_tmp.path().join("hooks");
    fs::create_dir_all(&hooks_dir).expect("create hooks dir");

    let ok_hook_path = hooks_dir.join("valid.sh");
    fs::write(&ok_hook_path, "#!/bin/bash\necho test").expect("write hook script");

    // Make the script executable on Unix.
    fs::set_permissions(&ok_hook_path, fs::Permissions::from_mode(0o755)).expect("chmod +x hook");

    let settings_path = home_claude_dir.join("settings.json");
    let hook_abs_path = ok_hook_path.to_string_lossy().to_string();
    fs::write(&settings_path, settings_json_with_hook(&hook_abs_path))
        .expect("write settings.json");

    // Spawn with HOME pointing to home_tmp and cwd pointing to an empty work_tmp.
    let output = Command::new(annulus_bin())
        .arg("validate-hooks")
        .env("HOME", home_tmp.path())
        .current_dir(work_tmp.path())
        .output()
        .expect("run annulus validate-hooks");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    // Verify exit code is 0 (success) because all hooks are OK.
    assert!(
        output.status.success(),
        "expected exit code 0 for valid hook, got: {0:?}",
        output.status.code()
    );

    // Verify the hook path appears on stdout.
    assert!(
        stdout.contains(hook_abs_path.as_str()),
        "expected stdout to contain hook path '{hook_abs_path}', got: '{stdout}'"
    );

    // Verify [OK] label appears on stderr.
    assert!(
        stderr.contains("[OK]"),
        "expected stderr to contain '[OK]' label, got: '{stderr}'"
    );

    // Verify no labels or prose appear on stdout.
    let stdout_lines: Vec<&str> = stdout.trim().lines().collect();
    assert_eq!(
        stdout_lines.len(),
        1,
        "expected exactly 1 line on stdout (the hook path), got: {stdout_lines:?}"
    );
    assert_eq!(
        stdout_lines[0], hook_abs_path,
        "expected stdout to be exactly the hook path"
    );
}

/// Test: no prose (labels like [OK], [STALE], or summary text) ever appears on stdout.
/// Stdout is strictly the resolved hook paths, one per line.
#[test]
fn test_stdout_contains_only_paths_no_prose() {
    let home_tmp = tempfile::TempDir::new().expect("create home tempdir");
    let work_tmp = tempfile::TempDir::new().expect("create work tempdir");

    let home_claude_dir = home_tmp.path().join(".claude");
    fs::create_dir_all(&home_claude_dir).expect("create .claude dir");

    let settings_path = home_claude_dir.join("settings.json");
    let hook_path = home_tmp.path().join("hooks").join("missing.sh");
    let hook_abs_path = hook_path.to_string_lossy().to_string();
    fs::write(&settings_path, settings_json_with_hook(&hook_abs_path))
        .expect("write settings.json");

    let output = Command::new(annulus_bin())
        .arg("validate-hooks")
        .env("HOME", home_tmp.path())
        .current_dir(work_tmp.path())
        .output()
        .expect("run annulus validate-hooks");

    let stdout = String::from_utf8_lossy(&output.stdout);

    // The fixture hook is stale, so the contract requires exit code exactly 1.
    assert_eq!(
        output.status.code(),
        Some(1),
        "expected exit code exactly 1 for stale hook, got: {:?}",
        output.status.code()
    );

    // Verify stdout contains only the path and no labels.
    assert!(
        !stdout.contains("[STALE]"),
        "expected [STALE] to be on stderr only, not stdout: '{stdout}'"
    );
    assert!(
        !stdout.contains("[OK]"),
        "expected [OK] to be on stderr only, not stdout: '{stdout}'"
    );
    assert!(
        !stdout.contains("[BROKEN]"),
        "expected [BROKEN] to be on stderr only, not stdout: '{stdout}'"
    );
    assert!(
        !stdout.contains("Checking"),
        "expected 'Checking' prose to be on stderr only, not stdout: '{stdout}'"
    );
    assert!(
        !stdout.contains("checked"),
        "expected summary prose to be on stderr only, not stdout: '{stdout}'"
    );

    // Stdout should contain exactly the hook path.
    assert!(
        stdout.contains(&hook_abs_path),
        "expected stdout to contain the hook path, got: '{stdout}'"
    );

    // Stdout must be exactly one line (the single hook path) — no extra output.
    assert_eq!(
        stdout.trim().lines().count(),
        1,
        "expected exactly 1 line on stdout, got: '{stdout}'"
    );
}

/// Test: when no settings files exist (completely empty HOME and cwd),
/// the process exits 0 with a message on stderr but nothing on stdout.
#[test]
fn test_no_settings_files_exits_0_stderr_message_empty_stdout() {
    let home_tmp = tempfile::TempDir::new().expect("create home tempdir");
    let work_tmp = tempfile::TempDir::new().expect("create work tempdir");

    // Both tempdirs are empty; no .claude/settings.json anywhere.

    let output = Command::new(annulus_bin())
        .arg("validate-hooks")
        .env("HOME", home_tmp.path())
        .current_dir(work_tmp.path())
        .output()
        .expect("run annulus validate-hooks");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    // Verify exit code is 0 (no hooks to fail).
    assert!(
        output.status.success(),
        "expected exit code 0 when no settings files exist, got: {0:?}",
        output.status.code()
    );

    // Verify stdout is empty.
    assert!(
        stdout.trim().is_empty(),
        "expected empty stdout when no settings files exist, got: '{stdout}'"
    );

    // Verify stderr contains the "No settings files found" message.
    assert!(
        stderr.contains("No Claude Code settings files found"),
        "expected stderr message about no settings files, got: '{stderr}'"
    );
}
