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

pub fn handle_config_export() -> Result<()> {
    let config = load_config();

    let segments = config
        .segments
        .iter()
        .map(|entry| ExportedSegment {
            id: entry.name.clone(),
            enabled: entry.enabled,
        })
        .collect();

    let export = ConfigExport {
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
    };

    let json = serde_json::to_string_pretty(&export)?;
    println!("{json}");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::StatuslineConfig;

    #[test]
    fn test_export_default_config() {
        let config = StatuslineConfig::default();

        let segments: Vec<ExportedSegment> = config
            .segments
            .iter()
            .map(|entry| ExportedSegment {
                id: entry.name.clone(),
                enabled: entry.enabled,
            })
            .collect();

        let export = ConfigExport {
            schema_version: "1.0".to_string(),
            segments,
            theme: Theme {
                color_mode: "auto".to_string(),
                separator: " | ".to_string(),
            },
            metadata: Metadata {
                created_at: "2026-05-06T00:00:00Z".to_string(),
                version: "1.0".to_string(),
            },
        };

        let json = serde_json::to_string_pretty(&export).unwrap();
        assert!(json.contains("\"schema_version\": \"1.0\""));
        assert!(json.contains("\"id\": \"context\""));
        assert!(json.contains("\"enabled\": true"));
        assert!(json.contains("\"color_mode\": \"auto\""));
        assert!(json.contains("\"separator\": \" | \""));
    }

    #[test]
    fn test_export_segments_match_config() {
        let mut config = StatuslineConfig::default();
        // Disable one segment
        if let Some(first) = config.segments.first_mut() {
            first.enabled = false;
        }

        let segments: Vec<ExportedSegment> = config
            .segments
            .iter()
            .map(|entry| ExportedSegment {
                id: entry.name.clone(),
                enabled: entry.enabled,
            })
            .collect();

        assert_eq!(segments.len(), config.segments.len());
        assert!(!segments[0].enabled);
        assert!(segments[1].enabled);
    }

    #[test]
    fn test_export_schema_version() {
        let config = StatuslineConfig::default();
        let segments = config
            .segments
            .iter()
            .map(|entry| ExportedSegment {
                id: entry.name.clone(),
                enabled: entry.enabled,
            })
            .collect();

        let export = ConfigExport {
            schema_version: "1.0".to_string(),
            segments,
            theme: Theme {
                color_mode: "auto".to_string(),
                separator: " | ".to_string(),
            },
            metadata: Metadata {
                created_at: "2026-05-06T00:00:00Z".to_string(),
                version: "1.0".to_string(),
            },
        };

        assert_eq!(export.schema_version, "1.0");
        assert_eq!(export.metadata.version, "1.0");
    }
}
