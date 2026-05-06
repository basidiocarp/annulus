use anyhow::Result;
use serde::Serialize;

use crate::config::load_config;

#[derive(Debug, Serialize)]
struct ExportedSegment {
    id: String,
    enabled: bool,
}

#[derive(Debug, Serialize)]
struct Theme {
    color_mode: String,
    separator: String,
}

#[derive(Debug, Serialize)]
struct Metadata {
    created_at: String,
    version: String,
}

#[derive(Debug, Serialize)]
struct ConfigExport {
    schema_version: String,
    segments: Vec<ExportedSegment>,
    theme: Theme,
    metadata: Metadata,
}

fn build_export(config: &crate::config::StatuslineConfig) -> ConfigExport {
    let segments = config
        .segments
        .iter()
        .map(|entry| ExportedSegment {
            id: entry.name.clone(),
            enabled: entry.enabled,
        })
        .collect();

    ConfigExport {
        schema_version: "1.0".to_string(),
        segments,
        theme: Theme {
            color_mode: "auto".to_string(),
            separator: " | ".to_string(),
        },
        metadata: Metadata {
            created_at: chrono::Utc::now().to_rfc3339(),
            version: "1.0".to_string(),
        },
    }
}

pub fn handle_config_export() -> Result<()> {
    let config = load_config();
    let export = build_export(&config);
    let json = serde_json::to_string_pretty(&export)?;
    println!("{json}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::StatuslineConfig;

    #[test]
    fn export_default_config_contains_expected_fields() {
        let config = StatuslineConfig::default();
        let export = build_export(&config);

        assert_eq!(export.schema_version, "1.0");
        assert!(export.segments.iter().any(|s| s.id == "context"));
        assert!(export.segments.iter().all(|s| s.enabled));
        assert_eq!(export.theme.color_mode, "auto");
        assert_eq!(export.theme.separator, " | ");
    }

    #[test]
    fn export_preserves_disabled_segment_by_name() {
        let mut config = StatuslineConfig::default();
        let first_name = config.segments.first().map(|s| s.name.clone()).unwrap();
        if let Some(first) = config.segments.first_mut() {
            first.enabled = false;
        }

        let export = build_export(&config);

        assert_eq!(export.segments.len(), config.segments.len());
        let first_segment = export.segments.iter().find(|s| s.id == first_name).unwrap();
        assert!(!first_segment.enabled);
        // All other segments should still be enabled
        for seg in export.segments.iter().filter(|s| s.id != first_name) {
            assert!(seg.enabled, "segment '{}' should be enabled", seg.id);
        }
    }

    #[test]
    fn export_schema_version_is_stable() {
        let config = StatuslineConfig::default();
        let export = build_export(&config);

        assert_eq!(export.schema_version, "1.0");
        assert_eq!(export.metadata.version, "1.0");
    }
}
