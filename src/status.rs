use std::fmt::Write;

use serde::{Deserialize, Serialize};
use spore::availability::{AvailabilityReport, probe_all};

/// JSON response for the `annulus status --json` subcommand.
#[derive(Debug, Serialize, Deserialize)]
#[must_use]
pub struct StatusReport {
    pub schema: String,
    pub version: String,
    pub reports: Vec<ToolReport>,
}

/// Serializable representation of a single tool availability report.
#[derive(Debug, Serialize, Deserialize)]
pub struct ToolReport {
    pub tool: String,
    pub available: bool,
    pub tier: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub degraded_capabilities: Vec<String>,
}

impl ToolReport {
    /// Convert an `AvailabilityReport` to a `ToolReport`.
    fn from_availability_report(report: AvailabilityReport) -> Self {
        Self {
            tool: report.tool,
            available: report.available,
            tier: format!("{}", report.tier),
            reason: report.reason,
            degraded_capabilities: report.degraded_capabilities,
        }
    }
}

/// Fetch availability reports from all registered tools and render as JSON.
pub fn status_json() -> String {
    let reports = probe_all();
    let tool_reports = reports
        .into_iter()
        .map(ToolReport::from_availability_report)
        .collect();

    let status = StatusReport {
        schema: "annulus-status-v1".to_string(),
        version: "1".to_string(),
        reports: tool_reports,
    };

    serde_json::to_string_pretty(&status).unwrap_or_else(|_| {
        serde_json::json!({
            "schema": "annulus-status-v1",
            "version": "1",
            "reports": []
        })
        .to_string()
    })
}

/// Fetch availability reports and render as a human-readable table.
pub fn status_table() -> String {
    let reports = probe_all();

    if reports.is_empty() {
        return "No tools registered for availability checking.".to_string();
    }

    let mut output = String::new();
    output.push_str("Tool           Tier   Status\n");
    output.push_str("─────────────────────────────────────────────\n");

    for report in reports {
        let tier_str = format!("{}", report.tier);
        let status = if report.available {
            "OK".to_string()
        } else {
            format!(
                "DOWN ({})",
                report.reason.as_deref().unwrap_or("unknown reason")
            )
        };

        let _ = writeln!(output, "{:<14} {:<6} {}", report.tool, tier_str, status);
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use spore::availability::DegradationTier;

    /// Determine the highest-priority degradation tier present in reports.
    /// Returns None if all tools are available or reports is empty.
    fn highest_degraded_tier(reports: &[AvailabilityReport]) -> Option<DegradationTier> {
        let unavailable = reports.iter().filter(|r| !r.available);

        let mut has_tier1 = false;
        let mut has_tier2 = false;
        let mut has_tier3 = false;

        for report in unavailable {
            match report.tier {
                DegradationTier::Tier1 => has_tier1 = true,
                DegradationTier::Tier2 => has_tier2 = true,
                DegradationTier::Tier3 => has_tier3 = true,
                _ => {}
            }
        }

        if has_tier1 {
            Some(DegradationTier::Tier1)
        } else if has_tier2 {
            Some(DegradationTier::Tier2)
        } else if has_tier3 {
            Some(DegradationTier::Tier3)
        } else {
            None
        }
    }

    /// Count unavailable tools at each tier.
    fn count_unavailable_by_tier(reports: &[AvailabilityReport]) -> (usize, usize, usize) {
        let mut tier1 = 0;
        let mut tier2 = 0;
        let mut tier3 = 0;

        for report in reports.iter().filter(|r| !r.available) {
            match report.tier {
                DegradationTier::Tier1 => tier1 += 1,
                DegradationTier::Tier2 => tier2 += 1,
                DegradationTier::Tier3 => tier3 += 1,
                _ => {}
            }
        }

        (tier1, tier2, tier3)
    }

    #[test]
    fn test_status_json_structure() {
        let json = status_json();
        let parsed: Result<StatusReport, _> = serde_json::from_str(&json);
        assert!(
            parsed.is_ok(),
            "status_json should produce valid StatusReport JSON"
        );
        let report = parsed.unwrap();
        assert_eq!(report.schema, "annulus-status-v1");
        assert_eq!(report.version, "1");
        assert!(
            !report.reports.is_empty(),
            "should have at least one report"
        );
    }

    #[test]
    fn test_tool_report_serialization() {
        let tool_report = ToolReport {
            tool: "hyphae".to_string(),
            available: false,
            tier: "tier2".to_string(),
            reason: Some("binary not found on PATH".to_string()),
            degraded_capabilities: vec!["persistent memory".to_string()],
        };
        let json = serde_json::to_string(&tool_report).unwrap();
        assert!(json.contains("\"tool\":\"hyphae\""));
        assert!(json.contains("\"available\":false"));
        assert!(json.contains("\"tier\":\"tier2\""));
    }

    #[test]
    fn test_status_table_structure() {
        let table = status_table();
        assert!(table.contains("Tool"));
        assert!(table.contains("Tier"));
        assert!(table.contains("Status"));
    }

    #[test]
    fn test_highest_degraded_tier_empty() {
        let reports = vec![];
        assert_eq!(highest_degraded_tier(&reports), None);
    }

    #[test]
    fn test_highest_degraded_tier_all_available() {
        let reports = vec![AvailabilityReport {
            tool: "mycelium".to_string(),
            available: true,
            tier: DegradationTier::Tier1,
            reason: None,
            degraded_capabilities: vec![],
        }];
        assert_eq!(highest_degraded_tier(&reports), None);
    }

    #[test]
    fn test_highest_degraded_tier_priority() {
        let reports = vec![
            AvailabilityReport {
                tool: "cap".to_string(),
                available: false,
                tier: DegradationTier::Tier3,
                reason: Some("not found".to_string()),
                degraded_capabilities: vec![],
            },
            AvailabilityReport {
                tool: "hyphae".to_string(),
                available: false,
                tier: DegradationTier::Tier2,
                reason: Some("not found".to_string()),
                degraded_capabilities: vec![],
            },
            AvailabilityReport {
                tool: "mycelium".to_string(),
                available: true,
                tier: DegradationTier::Tier1,
                reason: None,
                degraded_capabilities: vec![],
            },
        ];
        assert_eq!(
            highest_degraded_tier(&reports),
            Some(DegradationTier::Tier2)
        );
    }

    #[test]
    fn test_count_unavailable_by_tier() {
        let reports = vec![
            AvailabilityReport {
                tool: "mycelium".to_string(),
                available: false,
                tier: DegradationTier::Tier1,
                reason: Some("not found".to_string()),
                degraded_capabilities: vec![],
            },
            AvailabilityReport {
                tool: "hyphae".to_string(),
                available: false,
                tier: DegradationTier::Tier2,
                reason: Some("not found".to_string()),
                degraded_capabilities: vec![],
            },
            AvailabilityReport {
                tool: "cap".to_string(),
                available: false,
                tier: DegradationTier::Tier3,
                reason: Some("not found".to_string()),
                degraded_capabilities: vec![],
            },
        ];
        let (t1, t2, t3) = count_unavailable_by_tier(&reports);
        assert_eq!(t1, 1);
        assert_eq!(t2, 1);
        assert_eq!(t3, 1);
    }
}
