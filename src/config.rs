use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use serde::Deserialize;

const DEFAULT_CONTEXT_LIMIT: usize = 200_000;

const DEFAULT_SEGMENTS: &[&str] = &[
    "context",
    "usage",
    "cost",
    "model",
    "savings",
    "degradation",
    "branch",
    "workspace",
    "context-bar",
    "hyphae",
    // "cortina" is intentionally excluded from the default set until a direct
    // data seam is available. It can be re-enabled explicitly via config.
];

#[derive(Debug, Clone, Deserialize)]
pub struct SegmentEntry {
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize, Default)]
struct RawConfig {
    #[serde(default)]
    segments: Vec<SegmentEntry>,
    #[serde(default, rename = "context-limits")]
    context_limits: HashMap<String, usize>,
    /// Explicit provider override ("claude", "codex", "gemini").
    /// When absent, provider auto-detection is used (currently always claude).
    #[serde(default)]
    provider: Option<String>,
}

#[derive(Debug, Clone)]
pub struct StatuslineConfig {
    pub segments: Vec<SegmentEntry>,
    pub context_limits: HashMap<String, usize>,
    /// Explicit provider name. `None` means auto-detect (currently claude).
    pub provider: Option<String>,
}

impl Default for StatuslineConfig {
    fn default() -> Self {
        Self {
            segments: DEFAULT_SEGMENTS
                .iter()
                .map(|&name| SegmentEntry {
                    name: name.to_string(),
                    enabled: true,
                })
                .collect(),
            context_limits: HashMap::new(),
            provider: None,
        }
    }
}

impl StatuslineConfig {
    pub fn context_limit_for_model(&self, model: &str) -> usize {
        let normalized = model.to_ascii_lowercase();
        for (key, &limit) in &self.context_limits {
            if normalized.contains(&key.to_ascii_lowercase()) {
                return limit;
            }
        }
        DEFAULT_CONTEXT_LIMIT
    }
}

pub fn load_config() -> StatuslineConfig {
    let path = config_path();
    let Ok(contents) = fs::read_to_string(&path) else {
        return StatuslineConfig::default();
    };

    match toml::from_str::<RawConfig>(&contents) {
        Ok(raw) => {
            let segments = if raw.segments.is_empty() {
                StatuslineConfig::default().segments
            } else {
                for entry in &raw.segments {
                    if !DEFAULT_SEGMENTS.contains(&entry.name.as_str()) {
                        eprintln!("annulus: unknown segment name '{}' in config", entry.name);
                    }
                }
                raw.segments
            };
            StatuslineConfig {
                segments,
                context_limits: raw.context_limits,
                provider: raw.provider,
            }
        }
        Err(e) => {
            eprintln!("annulus: failed to parse {}: {e}", path.display());
            StatuslineConfig::default()
        }
    }
}

fn config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from(".config"))
        .join("annulus")
        .join("statusline.toml")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config_has_all_segments() {
        let config = StatuslineConfig::default();
        assert_eq!(config.segments.len(), 10);
        assert!(config.segments.iter().all(|s| s.enabled));
        let names: Vec<_> = config.segments.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "context",
                "usage",
                "cost",
                "model",
                "savings",
                "degradation",
                "branch",
                "workspace",
                "context-bar",
                "hyphae",
            ]
        );
    }

    #[test]
    fn test_load_config_missing_file_returns_defaults() {
        let config = load_config();
        assert_eq!(config.segments.len(), 10);
        assert!(config.segments.iter().all(|s| s.enabled));
    }

    #[test]
    fn test_parse_valid_config() {
        let toml_str = r#"
[[segments]]
name = "context"
enabled = true

[[segments]]
name = "usage"
enabled = false

[context-limits]
opus = 300000
sonnet = 250000
"#;
        let raw: RawConfig = toml::from_str(toml_str).expect("failed to parse TOML");
        assert_eq!(raw.segments.len(), 2);
        assert_eq!(raw.segments[0].name, "context");
        assert!(raw.segments[0].enabled);
        assert_eq!(raw.segments[1].name, "usage");
        assert!(!raw.segments[1].enabled);
        assert_eq!(raw.context_limits.get("opus"), Some(&300_000));
        assert_eq!(raw.context_limits.get("sonnet"), Some(&250_000));
    }

    #[test]
    fn test_parse_malformed_config_returns_defaults() {
        let malformed = "invalid toml [[[ content";
        let result = toml::from_str::<RawConfig>(malformed);
        assert!(result.is_err());
        let config = load_config();
        assert_eq!(config.segments.len(), 10);
    }

    #[test]
    fn test_context_limit_for_model() {
        let mut config = StatuslineConfig::default();
        config.context_limits.insert("opus".to_string(), 300_000);
        config.context_limits.insert("sonnet".to_string(), 250_000);

        assert_eq!(config.context_limit_for_model("opus"), 300_000);
        assert_eq!(config.context_limit_for_model("sonnet"), 250_000);
        assert_eq!(
            config.context_limit_for_model("haiku"),
            DEFAULT_CONTEXT_LIMIT
        );
    }

    #[test]
    fn test_unknown_segments_are_kept() {
        let toml_str = r#"
[[segments]]
name = "context"
enabled = true

[[segments]]
name = "custom-widget"
enabled = true

[[segments]]
name = "usage"
enabled = false
"#;
        let raw: RawConfig = toml::from_str(toml_str).expect("valid TOML");
        assert_eq!(raw.segments.len(), 3);
        assert_eq!(raw.segments[1].name, "custom-widget");
        assert!(raw.segments[1].enabled);
    }

    #[test]
    fn test_context_limit_for_model_case_insensitive() {
        let mut config = StatuslineConfig::default();
        config.context_limits.insert("opus".to_string(), 300_000);

        assert_eq!(config.context_limit_for_model("Opus"), 300_000);
        assert_eq!(config.context_limit_for_model("OPUS"), 300_000);
        assert_eq!(config.context_limit_for_model("claude-opus-4"), 300_000);
    }
}
