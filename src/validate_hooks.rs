use anyhow::{Context, Result};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

/// Represents the validation status of a hook path
#[derive(Debug, Clone, PartialEq, Eq)]
enum ValidationStatus {
    Ok,
    Stale,
    Broken,
}

/// Finds all Claude Code settings files to check
fn find_settings_files() -> Vec<PathBuf> {
    let mut files = Vec::new();

    // User scope: ~/.claude/settings.json
    if let Some(home) = dirs::home_dir() {
        files.push(home.join(".claude/settings.json"));
    }

    // Project scope: ./.claude/settings.json
    if let Ok(cwd) = std::env::current_dir() {
        files.push(cwd.join(".claude/settings.json"));
    }

    // Local scope: ./.claude/settings.local.json
    if let Ok(cwd) = std::env::current_dir() {
        files.push(cwd.join(".claude/settings.local.json"));
    }

    files
}

/// Parses hook entries from a settings file
fn parse_hook_entries(path: &Path) -> Result<Vec<(String, String, String)>> {
    let content =
        fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;

    if content.trim().is_empty() {
        return Ok(Vec::new());
    }

    let root: Value =
        serde_json::from_str(&content).with_context(|| format!("parsing {}", path.display()))?;

    let mut entries = Vec::new();

    if let Some(hooks) = root.get("hooks").and_then(Value::as_object) {
        for (event, event_entries) in hooks {
            let Some(event_entries) = event_entries.as_array() else {
                continue;
            };

            for entry in event_entries {
                let matcher = entry.get("matcher").and_then(Value::as_str).unwrap_or("*");

                let Some(command_entries) = entry.get("hooks").and_then(Value::as_array) else {
                    continue;
                };

                for command_entry in command_entries {
                    let Some(command) = command_entry.get("command").and_then(Value::as_str) else {
                        continue;
                    };

                    // Format: "Event → matcher → command" for grouping and display
                    entries.push((event.clone(), matcher.to_string(), command.to_string()));
                }
            }
        }
    }

    // Also check statusLine
    if let Some(status_line) = root.get("statusLine") {
        if let Some(command) = status_line.get("command").and_then(Value::as_str) {
            entries.push((
                "statusLine".to_string(),
                "*".to_string(),
                command.to_string(),
            ));
        }
    }

    Ok(entries)
}

/// Extracts absolute paths from a command string
fn extract_paths_from_command(command: &str) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let home = dirs::home_dir();

    for token in command.split_whitespace() {
        let candidate = token.trim_matches(|ch| matches!(ch, '"' | '\''));

        let path = if let Some(suffix) = candidate.strip_prefix("$HOME/") {
            home.as_ref().map(|home| home.join(suffix))
        } else if let Some(suffix) = candidate.strip_prefix("~/") {
            home.as_ref().map(|home| home.join(suffix))
        } else if candidate.starts_with('/') {
            Some(PathBuf::from(candidate))
        } else {
            None
        };

        if let Some(path) = path {
            if path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| matches!(ext, "js" | "sh" | "py"))
            {
                paths.push(path);
            }
        }
    }

    paths
}

/// Validates whether a hook path is executable
fn validate_hook_path(path: &Path) -> ValidationStatus {
    if !path.exists() {
        return ValidationStatus::Stale;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(metadata) = fs::metadata(path) {
            let permissions = metadata.permissions();
            let mode = permissions.mode();
            // Check if any execute bit is set (owner, group, or other)
            if (mode & 0o111) != 0 {
                return ValidationStatus::Ok;
            }
        }
        ValidationStatus::Broken
    }

    #[cfg(not(unix))]
    {
        // On non-unix, if the file exists, assume it's executable
        ValidationStatus::Ok
    }
}

/// Main entry point for the validate-hooks command
#[allow(clippy::unnecessary_wraps)]
pub fn run() -> Result<()> {
    let settings_files = find_settings_files();
    let existing_files: Vec<_> = settings_files
        .iter()
        .filter(|f| f.exists())
        .cloned()
        .collect();

    if existing_files.is_empty() {
        println!("No Claude Code settings files found.");
        return Ok(());
    }

    let mut any_failed = false;
    let mut stale_count = 0;
    let mut broken_count = 0;

    for settings_path in &existing_files {
        println!("Checking {}...", settings_path.display());

        let entries = match parse_hook_entries(settings_path) {
            Ok(entries) => entries,
            Err(e) => {
                eprintln!("  Error parsing {}: {e}", settings_path.display());
                any_failed = true;
                continue;
            }
        };

        if entries.is_empty() {
            println!("  (no hook commands found)");
            continue;
        }

        for (event, _matcher, command) in entries {
            let paths = extract_paths_from_command(&command);

            if paths.is_empty() {
                // No absolute paths to validate in this command
                println!("  [OK]    {event} → {command}");
                continue;
            }

            for path in paths {
                let status = validate_hook_path(&path);

                match status {
                    ValidationStatus::Ok => {
                        println!("  [OK]      {event} → {command}");
                    }
                    ValidationStatus::Stale => {
                        println!("  [STALE]   {event} → {command} (not found)");
                        stale_count += 1;
                        any_failed = true;
                    }
                    ValidationStatus::Broken => {
                        println!("  [BROKEN]  {event} → {command} (not executable)");
                        broken_count += 1;
                        any_failed = true;
                    }
                }
            }
        }
    }

    if stale_count > 0 || broken_count > 0 {
        println!(
            "\n{} file{} checked, {} stale, {} broken",
            existing_files.len(),
            if existing_files.len() == 1 { "" } else { "s" },
            stale_count,
            broken_count
        );
    } else {
        println!(
            "\n{} file{} checked, all hooks valid",
            existing_files.len(),
            if existing_files.len() == 1 { "" } else { "s" }
        );
    }

    if any_failed {
        std::process::exit(1);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_file(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("annulus-{name}-{unique}.json"))
    }

    #[test]
    fn test_extract_paths_from_command_absolute_paths() {
        let cmd = "cortina adapter /usr/local/bin/script.sh pre-tool-use";
        let paths = extract_paths_from_command(cmd);
        assert_eq!(paths, vec![PathBuf::from("/usr/local/bin/script.sh")]);
    }

    #[test]
    fn test_extract_paths_from_command_home_expansion() {
        let cmd = "handler ~/scripts/hook.sh";
        let paths = extract_paths_from_command(cmd);
        assert_eq!(paths.len(), 1);
        assert!(paths[0].ends_with("scripts/hook.sh"));
    }

    #[test]
    fn test_extract_paths_from_command_dollar_home() {
        let cmd = "handler $HOME/.local/bin/cortina";
        let paths = extract_paths_from_command(cmd);
        assert!(paths.is_empty()); // No script extension, so not extracted
    }

    #[test]
    fn test_extract_paths_from_command_ignores_relative_commands() {
        let cmd = "cortina adapter claude-code pre-tool-use";
        let paths = extract_paths_from_command(cmd);
        assert!(paths.is_empty());
    }

    #[test]
    fn test_extract_paths_from_command_quoted_paths() {
        let cmd = "handler \"/path/to/script.sh\" arg";
        let paths = extract_paths_from_command(cmd);
        assert_eq!(paths, vec![PathBuf::from("/path/to/script.sh")]);
    }

    #[test]
    fn test_parse_hook_entries_empty_file() {
        let path = temp_file("empty");
        fs::write(&path, "").unwrap();

        let entries = parse_hook_entries(&path).unwrap();
        assert_eq!(entries, vec![]);

        fs::remove_file(path).unwrap();
    }

    #[test]
    fn test_parse_hook_entries_valid_hooks() {
        let path = temp_file("valid");
        let json = serde_json::json!({
            "hooks": {
                "PreToolUse": [
                    {
                        "matcher": "Bash",
                        "hooks": [
                            {
                                "type": "command",
                                "command": "cortina adapter claude-code pre-tool-use",
                                "timeout": 2
                            }
                        ]
                    }
                ],
                "PostToolUse": [
                    {
                        "matcher": "Edit",
                        "hooks": [
                            {
                                "type": "command",
                                "command": "handler /path/to/hook.sh",
                                "timeout": 5
                            }
                        ]
                    }
                ]
            }
        });
        fs::write(&path, json.to_string()).unwrap();

        let entries = parse_hook_entries(&path).unwrap();
        assert_eq!(entries.len(), 2);

        // Check both events are present (order is not guaranteed due to JSON object iteration)
        let events: std::collections::HashSet<_> = entries.iter().map(|e| e.0.as_str()).collect();
        assert!(events.contains("PreToolUse"));
        assert!(events.contains("PostToolUse"));

        fs::remove_file(path).unwrap();
    }

    #[test]
    fn test_parse_hook_entries_with_statusline() {
        let path = temp_file("statusline");
        let json = serde_json::json!({
            "statusLine": {
                "type": "command",
                "command": "cortina statusline"
            }
        });
        fs::write(&path, json.to_string()).unwrap();

        let entries = parse_hook_entries(&path).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0, "statusLine");
        assert_eq!(entries[0].2, "cortina statusline");

        fs::remove_file(path).unwrap();
    }

    #[test]
    fn test_parse_hook_entries_missing_hooks_key() {
        let path = temp_file("no-hooks");
        let json = serde_json::json!({
            "other": "data"
        });
        fs::write(&path, json.to_string()).unwrap();

        let entries = parse_hook_entries(&path).unwrap();
        assert_eq!(entries, vec![]);

        fs::remove_file(path).unwrap();
    }

    #[test]
    fn test_validate_hook_path_stale() {
        let path = PathBuf::from("/nonexistent/path/to/script.sh");
        let status = validate_hook_path(&path);
        assert_eq!(status, ValidationStatus::Stale);
    }

    #[cfg(unix)]
    #[test]
    fn test_validate_hook_path_broken_not_executable() {
        use std::os::unix::fs::PermissionsExt;

        let path = temp_file("not-executable.sh");
        fs::write(&path, "#!/bin/bash\necho test").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();

        let status = validate_hook_path(&path);
        assert_eq!(status, ValidationStatus::Broken);

        fs::remove_file(path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn test_validate_hook_path_ok_executable() {
        use std::os::unix::fs::PermissionsExt;

        let path = temp_file("executable.sh");
        fs::write(&path, "#!/bin/bash\necho test").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();

        let status = validate_hook_path(&path);
        assert_eq!(status, ValidationStatus::Ok);

        fs::remove_file(path).unwrap();
    }
}
