// Bridge file reader for annulus flag-file state.
//
// Writer contract:
//   Path:   ~/.config/annulus/bridge.json
//   Schema: { "entries": [{ "key": "mode", "value": "focus", "ttl_secs": 300 }] }
//   Staleness: each entry's written_at + ttl_secs is checked; stale entries are dropped.
//              written_at is a Unix timestamp (seconds). If absent, entry never expires.
//   Missing/unreadable file → empty state, no error.

use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;

const MAX_BRIDGE_MESSAGE_BYTES: usize = 4 * 1024 * 1024; // 4 MiB

#[derive(Debug, Clone, Deserialize)]
pub struct BridgeEntry {
    pub key: String,
    pub value: String,
    #[serde(default)]
    pub ttl_secs: Option<u64>,
    #[serde(default)]
    pub written_at: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct BridgeFile {
    #[serde(default)]
    entries: Vec<BridgeEntry>,
}

#[derive(Debug, Clone, Default)]
pub struct BridgeState {
    pub entries: Vec<BridgeEntry>,
}

pub fn bridge_path() -> PathBuf {
    match dirs::config_dir() {
        Some(config_dir) => config_dir.join("annulus").join("bridge.json"),
        None => bridge_path_home_fallback(),
    }
}

// Fallback path when dirs::config_dir() returns None: $HOME/.config/annulus/bridge.json,
// or /tmp/annulus-bridge.json when HOME is also absent.
fn bridge_path_home_fallback() -> PathBuf {
    bridge_path_from_home(std::env::var("HOME").ok().as_deref())
}

fn bridge_path_from_home(home: Option<&str>) -> PathBuf {
    match home {
        Some(h) => PathBuf::from(h).join(".config/annulus/bridge.json"),
        None => PathBuf::from("/tmp/annulus-bridge.json"),
    }
}

pub fn read_bridge(path: &std::path::Path) -> BridgeState {
    // Read file, parse JSON, filter stale entries
    let Ok(file) = fs::File::open(path) else {
        return BridgeState::default();
    };

    let mut contents = String::new();
    let mut limited_reader = file.take(MAX_BRIDGE_MESSAGE_BYTES as u64);
    if limited_reader.read_to_string(&mut contents).is_err() {
        return BridgeState::default();
    }

    let Ok(bridge_file) = serde_json::from_str::<BridgeFile>(&contents) else {
        return BridgeState::default();
    };

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let entries: Vec<BridgeEntry> = bridge_file
        .entries
        .into_iter()
        .filter(|e| {
            // If both written_at and ttl_secs are present, check staleness
            if let (Some(written_at), Some(ttl_secs)) = (e.written_at, e.ttl_secs) {
                now <= written_at.saturating_add(ttl_secs)
            } else {
                // No TTL means entry lives forever
                true
            }
        })
        .collect();

    BridgeState { entries }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[test]
    fn read_bridge_returns_empty_when_file_missing() {
        let nonexistent = PathBuf::from("/tmp/nonexistent_bridge_test_file_xyz.json");
        let state = read_bridge(&nonexistent);
        assert!(state.entries.is_empty());
    }

    #[test]
    fn read_bridge_filters_stale_entries() {
        use std::io::Write;

        let mut temp = NamedTempFile::new().unwrap();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        // One fresh entry, one stale entry
        let json = format!(
            r#"{{"entries": [{{"key": "fresh", "value": "yes", "ttl_secs": 300, "written_at": {}}}, {{"key": "stale", "value": "no", "ttl_secs": 10, "written_at": {}}}]}}"#,
            now,
            now - 100 // 100 seconds ago, TTL is 10 secs, so expired
        );

        temp.write_all(json.as_bytes()).unwrap();
        temp.flush().unwrap();

        let state = read_bridge(temp.path());
        assert_eq!(state.entries.len(), 1);
        assert_eq!(state.entries[0].key, "fresh");
    }

    #[test]
    fn read_bridge_returns_all_when_no_ttl() {
        use std::io::Write;

        let mut temp = NamedTempFile::new().unwrap();

        // Entries without ttl_secs or written_at are never filtered
        let json = r#"{"entries": [{"key": "permanent", "value": "always_here"}]}"#;

        temp.write_all(json.as_bytes()).unwrap();
        temp.flush().unwrap();

        let state = read_bridge(temp.path());
        assert_eq!(state.entries.len(), 1);
        assert_eq!(state.entries[0].key, "permanent");
    }

    #[test]
    fn bridge_path_from_home_builds_config_path() {
        let path = bridge_path_from_home(Some("/fake/home"));
        assert_eq!(
            path,
            std::path::PathBuf::from("/fake/home/.config/annulus/bridge.json"),
        );
    }

    #[test]
    fn bridge_path_from_home_falls_back_to_tmp_when_absent() {
        let path = bridge_path_from_home(None);
        assert_eq!(path, std::path::PathBuf::from("/tmp/annulus-bridge.json"));
    }
}
