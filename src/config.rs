use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use serde::Deserialize;

const DEFAULT_CONTEXT_LIMIT: usize = 200_000;

#[derive(Debug, Clone, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SeparatorStyle {
    #[default]
    Pipe,
    Space,
    None,
}

impl SeparatorStyle {
    #[allow(dead_code)]
    pub fn as_str(&self) -> &'static str {
        match self {
            SeparatorStyle::Pipe => " │ ",
            SeparatorStyle::Space => "  ",
            SeparatorStyle::None => "",
        }
    }
}

const KNOWN_PROVIDERS: &[&str] = &["claude", "codex", "gemini"];

/// All segment names accepted by the statusline, including those not in `DEFAULT_SEGMENTS`.
pub const ALL_SEGMENT_NAMES: &[&str] = &[
    "context",
    "usage",
    "cost",
    "model",
    "savings",
    "degradation",
    "branch",
    "workspace",
    "context-bar",
    "context-metrics",
    "hyphae",
    "heartbeat",
    "bridge",
    "canopy-adoption",
    "canopy-notifications",
    "cortina",
];

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
    "context-metrics",
    "hyphae",
    "heartbeat",
    // "cortina" is intentionally excluded from the default set until a direct
    // data seam is available. It can be re-enabled explicitly via config.
];

#[derive(Debug, Clone, Deserialize)]
pub struct SegmentEntry {
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub color: Option<String>,
    // Per-segment separator override — wired in a follow-on pass once the global
    // separator is settled. Declared here so existing configs can include the field.
    #[serde(default)]
    #[allow(dead_code)]
    pub separator: Option<SeparatorStyle>,
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
    /// Global separator style used between all segments on the same line.
    #[serde(default)]
    separator: SeparatorStyle,
}

#[derive(Debug, Clone)]
pub struct StatuslineConfig {
    pub segments: Vec<SegmentEntry>,
    pub context_limits: HashMap<String, usize>,
    /// Explicit provider name. `None` means auto-detect (currently claude).
    pub provider: Option<String>,
    /// Separator string inserted between rendered segments on the same line.
    pub separator: SeparatorStyle,
}

impl Default for StatuslineConfig {
    fn default() -> Self {
        Self {
            segments: DEFAULT_SEGMENTS
                .iter()
                .map(|&name| SegmentEntry {
                    name: name.to_string(),
                    enabled: true,
                    color: None,
                    separator: None,
                })
                .collect(),
            context_limits: HashMap::new(),
            provider: None,
            separator: SeparatorStyle::default(),
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
        builtin_context_limit_for_model(model).unwrap_or(DEFAULT_CONTEXT_LIMIT)
    }
}

fn builtin_context_limit_for_model(model: &str) -> Option<usize> {
    let normalized = model
        .trim()
        .to_ascii_lowercase()
        .replace('_', "-")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("-");

    if normalized.contains("gpt-5")
        || normalized.contains("gpt-5.2-codex")
        || normalized.contains("gpt-5.4")
    {
        Some(400_000)
    } else if normalized.contains("gemini-2.5-pro")
        || normalized.contains("gemini-2.5-flash")
        || normalized.contains("gemini-2.5-flash-lite")
    {
        Some(1_048_576)
    } else if normalized.contains("o3") || normalized.contains("o4-mini") {
        Some(200_000)
    } else {
        None
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
                    if !ALL_SEGMENT_NAMES.contains(&entry.name.as_str()) {
                        eprintln!("annulus: unknown segment name '{}' in config", entry.name);
                    }
                }
                raw.segments
            };
            if let Some(ref p) = raw.provider {
                if !KNOWN_PROVIDERS.contains(&p.as_str()) {
                    eprintln!(
                        "annulus: unrecognised provider '{p}' in config; falling back to auto-detect"
                    );
                }
            }
            StatuslineConfig {
                segments,
                context_limits: raw.context_limits,
                provider: raw.provider,
                separator: raw.separator,
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
    fn test_default_segments_is_subset_of_all_segment_names() {
        for default_seg in DEFAULT_SEGMENTS {
            assert!(
                ALL_SEGMENT_NAMES.contains(default_seg),
                "DEFAULT_SEGMENTS contains '{default_seg}' which is not in ALL_SEGMENT_NAMES"
            );
        }
    }

    #[test]
    fn test_default_config_has_all_segments() {
        let config = StatuslineConfig::default();
        assert_eq!(config.segments.len(), 12);
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
                "context-metrics",
                "hyphae",
                "heartbeat",
            ]
        );
    }

    #[test]
    fn test_load_config_missing_file_returns_defaults() {
        let config = load_config();
        assert_eq!(config.segments.len(), 12);
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
        assert_eq!(config.segments.len(), 12);
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

    #[test]
    fn test_builtin_context_limit_for_latest_openai_models() {
        let config = StatuslineConfig::default();
        assert_eq!(config.context_limit_for_model("GPT-5.4 mini"), 400_000);
        assert_eq!(config.context_limit_for_model("gpt-5.2-codex"), 400_000);
    }

    #[test]
    fn test_builtin_context_limit_for_latest_gemini_models() {
        let config = StatuslineConfig::default();
        assert_eq!(
            config.context_limit_for_model("Gemini 2.5 Flash"),
            1_048_576
        );
        assert_eq!(
            config.context_limit_for_model("gemini-2.5-flash-lite"),
            1_048_576
        );
    }

    #[test]
    fn test_custom_context_limit_overrides_builtin_limit() {
        let mut config = StatuslineConfig::default();
        config.context_limits.insert("gpt-5.4".to_string(), 500_000);

        assert_eq!(config.context_limit_for_model("GPT-5.4 mini"), 500_000);
    }

    #[test]
    fn all_segment_names_is_complete() {
        // This test verifies that ALL_SEGMENT_NAMES contains all segments that
        // are actually implemented in statusline.rs and vice versa.
        // If a segment is added without updating ALL_SEGMENT_NAMES, this test fails.
        let expected = vec![
            "context",
            "usage",
            "cost",
            "model",
            "savings",
            "degradation",
            "branch",
            "workspace",
            "context-bar",
            "context-metrics",
            "hyphae",
            "heartbeat",
            "bridge",
            "canopy-adoption",
            "canopy-notifications",
            "cortina",
        ];

        let mut all_seg_names = ALL_SEGMENT_NAMES.to_vec();
        all_seg_names.sort_unstable();

        let mut expected_sorted = expected;
        expected_sorted.sort_unstable();

        assert_eq!(
            all_seg_names, expected_sorted,
            "ALL_SEGMENT_NAMES is out of sync with registered segments"
        );
    }
}
