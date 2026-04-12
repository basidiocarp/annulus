use std::fs::File;
use std::io::{self, BufRead, BufReader, IsTerminal, Read};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Deserialize;
use serde_json::Value;

use crate::config::{StatuslineConfig, load_config};

const TIERED_PRICING_THRESHOLD: usize = 200_000;

#[derive(Debug, Default, Deserialize)]
struct StatuslineInput {
    #[serde(default)]
    transcript_path: Option<String>,
    #[serde(default)]
    model: Option<StatuslineModel>,
    #[serde(default)]
    workspace: Option<StatuslineWorkspace>,
}

#[derive(Debug, Default, Deserialize)]
struct StatuslineModel {
    #[serde(default)]
    display_name: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct StatuslineWorkspace {
    #[serde(default)]
    current_dir: Option<String>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
#[allow(
    clippy::struct_field_names,
    reason = "These names mirror Claude transcript usage fields"
)]
struct TokenUsage {
    input_tokens: usize,
    output_tokens: usize,
    cache_read_input_tokens: usize,
    cache_creation_input_tokens: usize,
}

impl TokenUsage {
    fn prompt_tokens(self) -> usize {
        self.input_tokens + self.cache_read_input_tokens + self.cache_creation_input_tokens
    }

    fn has_data(self) -> bool {
        self.input_tokens > 0
            || self.output_tokens > 0
            || self.cache_read_input_tokens > 0
            || self.cache_creation_input_tokens > 0
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct TranscriptUsage {
    requests: usize,
    cumulative: TokenUsage,
    latest_assistant: Option<TokenUsage>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
#[allow(
    clippy::struct_field_names,
    reason = "These names mirror the per-million pricing fields they represent"
)]
struct Pricing {
    input_per_million: f64,
    output_per_million: f64,
    cache_read_per_million: f64,
    cache_creation_per_million: f64,
    cache_read_above_threshold: f64,
    cache_creation_above_threshold: f64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SavingsStat {
    saved_tokens: usize,
    input_tokens: usize,
}

#[derive(Debug, Clone, PartialEq)]
struct StatuslineView {
    context_pct: Option<u8>,
    usage: Option<TokenUsage>,
    cost: Option<f64>,
    model_name: String,
    branch: Option<String>,
    workspace_name: Option<String>,
    savings: Option<SavingsStat>,
}

pub fn handle_stdin(no_color: bool) -> Result<()> {
    let stdin = io::stdin();
    if stdin.is_terminal() {
        render_and_print(StatuslineInput::default(), no_color);
        return Ok(());
    }

    let input = parse_statusline_input_from_reader(stdin.lock())?;
    render_and_print(input, no_color);
    Ok(())
}

fn parse_statusline_input_from_reader<R: Read>(reader: R) -> Result<StatuslineInput> {
    let mut deserializer = serde_json::Deserializer::from_reader(reader);
    match StatuslineInput::deserialize(&mut deserializer) {
        Ok(input) => Ok(input),
        Err(error) if error.is_eof() => Ok(StatuslineInput::default()),
        Err(error) => Err(error.into()),
    }
}

fn render_and_print(input: StatuslineInput, no_color: bool) {
    let config = load_config();
    let view = statusline_view(input, &config);
    let segments = segments_from_config(&config);
    println!("{}", render_statusline(&view, !no_color, &segments));
}

fn statusline_view(input: StatuslineInput, config: &StatuslineConfig) -> StatuslineView {
    let transcript_usage = input
        .transcript_path
        .as_deref()
        .and_then(|path| read_transcript_usage(path).ok());
    let usage = transcript_usage
        .map(|usage| usage.cumulative)
        .filter(|usage| usage.has_data());
    let model_name = input
        .model
        .and_then(|model| model.display_name)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    let pricing = pricing_for_model(&model_name);
    let context_limit = config.context_limit_for_model(&model_name);
    let context_pct = transcript_usage
        .and_then(|usage| usage.latest_assistant)
        .filter(|usage| usage.has_data())
        .map(|usage| context_pct_for_usage(usage, context_limit));
    let cost = usage
        .zip(pricing)
        .map(|(usage, pricing)| cost_for_usage(usage, pricing));
    let workspace_dir = input.workspace.and_then(|workspace| workspace.current_dir);
    let branch = workspace_dir.as_deref().and_then(git_branch_for_workspace);
    let workspace_name = workspace_dir
        .as_deref()
        .and_then(workspace_name_for_dir)
        .filter(|name| !name.is_empty());
    let savings = current_runtime_session_id()
        .as_deref()
        .and_then(|session_id| mycelium_session_savings(session_id).ok().flatten())
        .filter(|stat| stat.saved_tokens > 0);

    StatuslineView {
        context_pct,
        usage,
        cost,
        model_name: compact_model_name(&model_name),
        branch,
        workspace_name,
        savings,
    }
}

fn read_transcript_usage(path: &str) -> Result<TranscriptUsage> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut usage = TranscriptUsage::default();

    for line in reader.lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let Ok(entry) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };
        if !is_assistant_entry(&entry) {
            continue;
        }

        let usage_value = entry
            .get("message")
            .and_then(|message| message.get("usage"))
            .or_else(|| entry.get("usage"));
        let Some(usage_value) = usage_value else {
            continue;
        };

        let entry_usage = TokenUsage {
            input_tokens: usage_field(Some(usage_value), "input_tokens"),
            output_tokens: usage_field(Some(usage_value), "output_tokens"),
            cache_read_input_tokens: usage_field(Some(usage_value), "cache_read_input_tokens"),
            cache_creation_input_tokens: usage_field(
                Some(usage_value),
                "cache_creation_input_tokens",
            ),
        };
        usage.cumulative.input_tokens += entry_usage.input_tokens;
        usage.cumulative.output_tokens += entry_usage.output_tokens;
        usage.cumulative.cache_read_input_tokens += entry_usage.cache_read_input_tokens;
        usage.cumulative.cache_creation_input_tokens += entry_usage.cache_creation_input_tokens;
        usage.requests += 1;
        usage.latest_assistant = Some(entry_usage);
    }

    Ok(usage)
}

fn is_assistant_entry(entry: &Value) -> bool {
    entry.get("type").and_then(Value::as_str) == Some("assistant")
}

#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "Statusline percentage is presentation-only and explicitly clamped"
)]
fn context_pct_for_usage(usage: TokenUsage, context_limit: usize) -> u8 {
    let pct = ((usage.prompt_tokens() as f64 / context_limit as f64) * 100.0).round();
    pct.clamp(0.0, 255.0) as u8
}

fn usage_field(usage: Option<&Value>, field: &str) -> usize {
    usage
        .and_then(|value| value.get(field))
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(0)
}

fn pricing_for_model(display_name: &str) -> Option<Pricing> {
    let normalized = display_name.to_ascii_lowercase();
    if normalized.contains("opus") {
        Some(Pricing {
            input_per_million: 15.0,
            output_per_million: 75.0,
            cache_read_per_million: 1.5,
            cache_creation_per_million: 18.75,
            cache_read_above_threshold: 2.5,
            cache_creation_above_threshold: 25.0,
        })
    } else if normalized.contains("sonnet") {
        Some(Pricing {
            input_per_million: 3.0,
            output_per_million: 15.0,
            cache_read_per_million: 0.30,
            cache_creation_per_million: 3.75,
            cache_read_above_threshold: 0.50,
            cache_creation_above_threshold: 5.0,
        })
    } else if normalized.contains("haiku") {
        Some(Pricing {
            input_per_million: 0.80,
            output_per_million: 4.0,
            cache_read_per_million: 0.08,
            cache_creation_per_million: 1.0,
            cache_read_above_threshold: 0.13,
            cache_creation_above_threshold: 1.30,
        })
    } else {
        None
    }
}

#[allow(
    clippy::cast_precision_loss,
    reason = "Statusline cost is a coarse UI estimate derived from token counts"
)]
fn cost_for_usage(usage: TokenUsage, pricing: Pricing) -> f64 {
    let prompt_tokens = usage.prompt_tokens();
    let (cache_read_rate, cache_creation_rate) = if prompt_tokens > TIERED_PRICING_THRESHOLD {
        (
            pricing.cache_read_above_threshold,
            pricing.cache_creation_above_threshold,
        )
    } else {
        (
            pricing.cache_read_per_million,
            pricing.cache_creation_per_million,
        )
    };

    ((usage.input_tokens as f64 * pricing.input_per_million)
        + (usage.output_tokens as f64 * pricing.output_per_million)
        + (usage.cache_read_input_tokens as f64 * cache_read_rate)
        + (usage.cache_creation_input_tokens as f64 * cache_creation_rate))
        / 1_000_000.0
}

fn compact_model_name(display_name: &str) -> String {
    let normalized = display_name
        .trim()
        .to_ascii_lowercase()
        .replace("claude ", "");
    let compact = normalized.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.is_empty() {
        "unknown".to_string()
    } else {
        compact
    }
}

fn git_branch_for_workspace(cwd: &str) -> Option<String> {
    let cwd = Path::new(cwd);
    let output = Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!branch.is_empty() && branch != "HEAD").then_some(branch)
}

fn workspace_name_for_dir(cwd: &str) -> Option<String> {
    let path = Path::new(cwd);
    let name = path.file_name()?.to_str()?.trim();
    (!name.is_empty()).then(|| name.to_string())
}

fn current_runtime_session_id() -> Option<String> {
    spore::claude_session_id()
}

fn mycelium_session_savings(session_id: &str) -> Result<Option<SavingsStat>> {
    let db_path = mycelium_db_path()?;
    mycelium_session_savings_at_path(&db_path, session_id)
}

fn mycelium_session_savings_at_path(
    db_path: &Path,
    session_id: &str,
) -> Result<Option<SavingsStat>> {
    if !db_path.exists() {
        return Ok(None);
    }

    let conn = Connection::open(db_path)?;
    let row = conn
        .query_row(
            "SELECT COALESCE(SUM(saved_tokens), 0), COALESCE(SUM(input_tokens), 0)
             FROM commands
             WHERE session_id = ?1",
            params![session_id],
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "Mycelium stores non-negative token counts in SQLite INTEGER columns"
            )]
            |row| {
                Ok(SavingsStat {
                    saved_tokens: row.get::<_, i64>(0)? as usize,
                    input_tokens: row.get::<_, i64>(1)? as usize,
                })
            },
        )
        .optional()?;
    Ok(row)
}

fn mycelium_db_path() -> Result<PathBuf> {
    Ok(spore::paths::db_path(
        "mycelium",
        "history.db",
        "MYCELIUM_DB_PATH",
        None,
    )?)
}

trait Segment {
    #[allow(dead_code)]
    fn name(&self) -> &'static str;
    fn render(&self, view: &StatuslineView, color: bool) -> Option<String>;
    fn line(&self) -> u8;
}

struct ContextSegment;
impl Segment for ContextSegment {
    fn name(&self) -> &'static str {
        "context"
    }
    fn line(&self) -> u8 {
        1
    }
    fn render(&self, view: &StatuslineView, color: bool) -> Option<String> {
        let context = match view.context_pct {
            Some(pct) => format!("ctx: ▲ {pct}%"),
            None => "ctx: --".to_string(),
        };
        let context_code = match view.context_pct {
            Some(pct) if pct >= 85 => "31",
            Some(pct) if pct >= 60 => "33",
            Some(_) => "32",
            None => "2",
        };
        Some(paint(&context, context_code, color))
    }
}

struct UsageSegment;
impl Segment for UsageSegment {
    fn name(&self) -> &'static str {
        "usage"
    }
    fn line(&self) -> u8 {
        1
    }
    fn render(&self, view: &StatuslineView, color: bool) -> Option<String> {
        let usage = match view.usage {
            Some(usage) => format!(
                "in: {} • out: {} • cache: {}",
                format_tokens(usage.input_tokens),
                format_tokens(usage.output_tokens),
                format_tokens(usage.cache_read_input_tokens + usage.cache_creation_input_tokens)
            ),
            None => "--".to_string(),
        };
        Some(paint(&usage, "36", color))
    }
}

struct CostSegment;
impl Segment for CostSegment {
    fn name(&self) -> &'static str {
        "cost"
    }
    fn line(&self) -> u8 {
        1
    }
    fn render(&self, view: &StatuslineView, color: bool) -> Option<String> {
        let cost = view
            .cost
            .map_or_else(|| "--".to_string(), |cost| format!("${cost:.2}"));
        Some(paint(&cost, "35", color))
    }
}

struct ModelSegment;
impl Segment for ModelSegment {
    fn name(&self) -> &'static str {
        "model"
    }
    fn line(&self) -> u8 {
        2
    }
    fn render(&self, view: &StatuslineView, color: bool) -> Option<String> {
        Some(paint(&view.model_name, "34", color))
    }
}

struct SavingsSegment;
impl Segment for SavingsSegment {
    fn name(&self) -> &'static str {
        "savings"
    }
    fn line(&self) -> u8 {
        2
    }
    fn render(&self, view: &StatuslineView, color: bool) -> Option<String> {
        view.savings.as_ref().map(|savings| {
            paint(
                &format!("↓{} saved", format_tokens(savings.saved_tokens)),
                "32",
                color,
            )
        })
    }
}

struct BranchSegment;
impl Segment for BranchSegment {
    fn name(&self) -> &'static str {
        "branch"
    }
    fn line(&self) -> u8 {
        2
    }
    fn render(&self, view: &StatuslineView, color: bool) -> Option<String> {
        view.branch
            .as_ref()
            .map(|branch| paint(&format!("git: {branch}"), "2", color))
    }
}

struct WorkspaceSegment;
impl Segment for WorkspaceSegment {
    fn name(&self) -> &'static str {
        "workspace"
    }
    fn line(&self) -> u8 {
        2
    }
    fn render(&self, view: &StatuslineView, color: bool) -> Option<String> {
        view.workspace_name
            .as_ref()
            .map(|name| paint(&format!("ws: {name}"), "2", color))
    }
}

struct ContextBarSegment;
impl Segment for ContextBarSegment {
    fn name(&self) -> &'static str {
        "context-bar"
    }
    fn line(&self) -> u8 {
        1
    }
    fn render(&self, view: &StatuslineView, color: bool) -> Option<String> {
        let bar_width = 12;

        let (filled, label, color_code) = match view.context_pct {
            Some(pct) => {
                let fill = if pct > 0 {
                    ((usize::from(pct) * bar_width) / 100).max(1)
                } else {
                    0
                };
                let fill = fill.min(bar_width);
                let code = if pct >= 85 {
                    "31"
                } else if pct >= 60 {
                    "33"
                } else {
                    "32"
                };
                (fill, format!("{pct}%"), code)
            }
            None => (0, "--".to_string(), "2"),
        };

        let bar: String = "█".repeat(filled) + &"░".repeat(bar_width - filled);
        Some(paint(&format!("[ctx {bar} {label}]"), color_code, color))
    }
}

fn default_segments() -> Vec<Box<dyn Segment>> {
    vec![
        Box::new(ContextSegment),
        Box::new(UsageSegment),
        Box::new(CostSegment),
        Box::new(ModelSegment),
        Box::new(SavingsSegment),
        Box::new(BranchSegment),
        Box::new(WorkspaceSegment),
        Box::new(ContextBarSegment),
    ]
}

fn segments_from_config(config: &StatuslineConfig) -> Vec<Box<dyn Segment>> {
    if config.segments.is_empty() {
        return default_segments();
    }

    let mut segments: Vec<Box<dyn Segment>> = vec![];
    for entry in &config.segments {
        if !entry.enabled {
            continue;
        }
        let segment: Option<Box<dyn Segment>> = match entry.name.as_str() {
            "context" => Some(Box::new(ContextSegment)),
            "usage" => Some(Box::new(UsageSegment)),
            "cost" => Some(Box::new(CostSegment)),
            "model" => Some(Box::new(ModelSegment)),
            "savings" => Some(Box::new(SavingsSegment)),
            "branch" => Some(Box::new(BranchSegment)),
            "workspace" => Some(Box::new(WorkspaceSegment)),
            "context-bar" => Some(Box::new(ContextBarSegment)),
            _ => None,
        };
        if let Some(seg) = segment {
            segments.push(seg);
        }
    }

    segments
}

fn render_statusline(view: &StatuslineView, color: bool, segments: &[Box<dyn Segment>]) -> String {
    let line_one: Vec<String> = segments
        .iter()
        .filter(|s| s.line() == 1)
        .filter_map(|s| s.render(view, color))
        .collect();
    let line_two: Vec<String> = segments
        .iter()
        .filter(|s| s.line() == 2)
        .filter_map(|s| s.render(view, color))
        .collect();

    let line_one = line_one.join(" │ ");
    let line_two = line_two.join(" │ ");

    if line_two.is_empty() {
        line_one
    } else {
        format!("{line_one}\n{line_two}")
    }
}

#[allow(
    clippy::cast_precision_loss,
    reason = "Compact token display only needs approximate decimal formatting"
)]
fn format_tokens(value: usize) -> String {
    if value >= 1_000_000 {
        format!("{:.1}M", value as f64 / 1_000_000.0)
    } else if value >= 1_000 {
        format!("{:.1}K", value as f64 / 1_000.0)
    } else {
        value.to_string()
    }
}

fn paint(value: &str, code: &str, color: bool) -> String {
    if color {
        format!("\u{1b}[{code}m{value}\u{1b}[0m")
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn compact_model_name_normalizes_claude_labels() {
        assert_eq!(compact_model_name("Claude Sonnet 4.6"), "sonnet 4.6");
        assert_eq!(compact_model_name("Claude Opus 4.6"), "opus 4.6");
        assert_eq!(compact_model_name(""), "unknown");
    }

    #[test]
    fn workspace_name_for_dir_uses_path_basename() {
        assert_eq!(
            workspace_name_for_dir("/workspace/basidiocarp"),
            Some("basidiocarp".to_string())
        );
        assert_eq!(workspace_name_for_dir("/"), None);
    }

    #[test]
    fn parse_statusline_input_from_reader_defaults_on_empty_input() {
        let input = parse_statusline_input_from_reader(std::io::Cursor::new(Vec::<u8>::new()))
            .expect("empty stdin should default");

        assert_eq!(input.transcript_path, None);
        assert!(input.model.is_none());
        assert!(input.workspace.is_none());
    }

    #[test]
    fn parse_statusline_input_from_reader_parses_single_json_value() {
        let input = parse_statusline_input_from_reader(std::io::Cursor::new(
            br#"{"model":{"display_name":"Claude Sonnet 4.6"},"workspace":{"current_dir":"/tmp"}}"#,
        ))
        .expect("stdin json should parse");

        assert_eq!(
            input.model.and_then(|model| model.display_name),
            Some("Claude Sonnet 4.6".to_string())
        );
        assert_eq!(
            input.workspace.and_then(|workspace| workspace.current_dir),
            Some("/tmp".to_string())
        );
    }

    #[test]
    fn read_transcript_usage_sums_assistant_usage() {
        let temp_dir = std::env::temp_dir().join("annulus-statusline-usage");
        let _ = fs::remove_dir_all(&temp_dir);
        fs::create_dir_all(&temp_dir).unwrap();
        let transcript = temp_dir.join("transcript.jsonl");
        fs::write(
            &transcript,
            concat!(
                "{\"type\":\"assistant\",\"message\":{\"usage\":{\"input_tokens\":1200,\"output_tokens\":300,\"cache_read_input_tokens\":500,\"cache_creation_input_tokens\":100}}}\n",
                "{\"type\":\"human\",\"text\":\"ignored\"}\n",
                "{\"type\":\"assistant\",\"usage\":{\"input_tokens\":800,\"output_tokens\":200,\"cache_read_input_tokens\":100,\"cache_creation_input_tokens\":50}}\n"
            ),
        )
        .unwrap();

        let usage = read_transcript_usage(transcript.to_str().unwrap()).unwrap();
        assert_eq!(
            usage,
            TranscriptUsage {
                requests: 2,
                cumulative: TokenUsage {
                    input_tokens: 2000,
                    output_tokens: 500,
                    cache_read_input_tokens: 600,
                    cache_creation_input_tokens: 150,
                },
                latest_assistant: Some(TokenUsage {
                    input_tokens: 800,
                    output_tokens: 200,
                    cache_read_input_tokens: 100,
                    cache_creation_input_tokens: 50,
                }),
            }
        );

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn read_transcript_usage_ignores_assistant_entries_without_usage_payload() {
        let temp_dir = tempfile::TempDir::new().expect("tempdir");
        let transcript = temp_dir.path().join("usage-gaps.jsonl");
        fs::write(
            &transcript,
            concat!(
                "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"no usage\"}]}}\n",
                "{\"type\":\"assistant\",\"usage\":{\"input_tokens\":300,\"output_tokens\":100,\"cache_read_input_tokens\":25,\"cache_creation_input_tokens\":50}}\n"
            ),
        )
        .expect("write transcript");

        let usage = read_transcript_usage(transcript.to_str().expect("utf8 path"))
            .expect("read transcript usage");

        assert_eq!(usage.requests, 1);
        assert_eq!(
            usage.cumulative,
            TokenUsage {
                input_tokens: 300,
                output_tokens: 100,
                cache_read_input_tokens: 25,
                cache_creation_input_tokens: 50,
            }
        );
    }

    #[test]
    fn statusline_view_uses_latest_turn_for_context_pct() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let transcript = temp_dir.path().join("transcript.jsonl");
        fs::write(
            &transcript,
            concat!(
                "{\"type\":\"assistant\",\"usage\":{\"input_tokens\":180000,\"output_tokens\":25000,\"cache_read_input_tokens\":50000,\"cache_creation_input_tokens\":10000}}\n",
                "{\"type\":\"assistant\",\"usage\":{\"input_tokens\":45000,\"output_tokens\":12000,\"cache_read_input_tokens\":80000,\"cache_creation_input_tokens\":9000}}\n"
            ),
        )
        .unwrap();

        let config = StatuslineConfig::default();
        let view = statusline_view(
            StatuslineInput {
                transcript_path: Some(transcript.to_string_lossy().to_string()),
                model: Some(StatuslineModel {
                    display_name: Some("Claude Sonnet 4.6".to_string()),
                }),
                workspace: None,
            },
            &config,
        );

        assert_eq!(view.context_pct, Some(67));
        assert_eq!(
            view.usage,
            Some(TokenUsage {
                input_tokens: 225_000,
                output_tokens: 37_000,
                cache_read_input_tokens: 130_000,
                cache_creation_input_tokens: 19_000,
            })
        );
    }

    #[test]
    fn statusline_view_uses_custom_context_limit_from_config() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let transcript = temp_dir.path().join("transcript.jsonl");
        fs::write(
            &transcript,
            "{\"type\":\"assistant\",\"usage\":{\"input_tokens\":50000,\"output_tokens\":10000,\"cache_read_input_tokens\":30000,\"cache_creation_input_tokens\":10000}}\n",
        ).unwrap();

        let mut config = StatuslineConfig::default();
        config.context_limits.insert("sonnet".to_string(), 100_000);

        let view = statusline_view(
            StatuslineInput {
                transcript_path: Some(transcript.to_string_lossy().to_string()),
                model: Some(StatuslineModel {
                    display_name: Some("Claude Sonnet 4.6".to_string()),
                }),
                workspace: None,
            },
            &config,
        );

        assert_eq!(view.context_pct, Some(90));
    }

    #[test]
    fn render_statusline_without_color_is_compact() {
        let segments = default_segments();
        let line = render_statusline(
            &StatuslineView {
                context_pct: Some(42),
                usage: Some(TokenUsage {
                    input_tokens: 45_000,
                    output_tokens: 12_000,
                    cache_read_input_tokens: 80_000,
                    cache_creation_input_tokens: 9_000,
                }),
                cost: Some(1.23),
                model_name: "sonnet 4.6".to_string(),
                branch: Some("main".to_string()),
                workspace_name: Some("basidiocarp".to_string()),
                savings: Some(SavingsStat {
                    saved_tokens: 8_200,
                    input_tokens: 10_000,
                }),
            },
            false,
            &segments,
        );

        assert_eq!(
            line,
            "ctx: ▲ 42% │ in: 45.0K • out: 12.0K • cache: 89.0K │ $1.23 │ [ctx █████░░░░░░░ 42%]\nsonnet 4.6 │ ↓8.2K saved │ git: main │ ws: basidiocarp"
        );
    }

    #[test]
    fn render_statusline_degrades_gracefully() {
        let segments = default_segments();
        let line = render_statusline(
            &StatuslineView {
                context_pct: None,
                usage: None,
                cost: None,
                model_name: "unknown".to_string(),
                branch: None,
                workspace_name: None,
                savings: None,
            },
            false,
            &segments,
        );

        assert_eq!(line, "ctx: -- │ -- │ -- │ [ctx ░░░░░░░░░░░░ --]\nunknown");
    }

    #[test]
    fn mycelium_session_savings_reads_sqlite() {
        let temp_dir = std::env::temp_dir().join("annulus-statusline-mycelium");
        let _ = fs::remove_dir_all(&temp_dir);
        fs::create_dir_all(&temp_dir).unwrap();
        let db_path = temp_dir.join("history.db");
        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
            "CREATE TABLE commands (
                session_id TEXT,
                input_tokens INTEGER NOT NULL,
                saved_tokens INTEGER NOT NULL
            )",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO commands (session_id, input_tokens, saved_tokens) VALUES (?1, ?2, ?3)",
            params!["session-123", 1200_i64, 800_i64],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO commands (session_id, input_tokens, saved_tokens) VALUES (?1, ?2, ?3)",
            params!["session-123", 300_i64, 100_i64],
        )
        .unwrap();

        let stat = mycelium_session_savings_at_path(&db_path, "session-123")
            .unwrap()
            .expect("session savings should exist");

        assert_eq!(stat.saved_tokens, 900);
        assert_eq!(stat.input_tokens, 1500);

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn cost_for_usage_below_threshold_uses_base_rates() {
        let usage = TokenUsage {
            input_tokens: 50_000,
            output_tokens: 10_000,
            cache_read_input_tokens: 80_000,
            cache_creation_input_tokens: 10_000,
        };
        let pricing = pricing_for_model("sonnet").unwrap();
        let cost = cost_for_usage(usage, pricing);
        // prompt_tokens = 50k + 80k + 10k = 140k (below 200k)
        // (50000*3 + 10000*15 + 80000*0.30 + 10000*3.75) / 1_000_000
        let expected =
            (50_000.0 * 3.0 + 10_000.0 * 15.0 + 80_000.0 * 0.30 + 10_000.0 * 3.75) / 1_000_000.0;
        assert!(
            (cost - expected).abs() < 0.001,
            "cost={cost}, expected={expected}"
        );
    }

    #[test]
    fn cost_for_usage_at_threshold_uses_base_rates() {
        let usage = TokenUsage {
            input_tokens: 100_000,
            output_tokens: 5_000,
            cache_read_input_tokens: 80_000,
            cache_creation_input_tokens: 20_000,
        };
        let pricing = pricing_for_model("sonnet").unwrap();
        let cost = cost_for_usage(usage, pricing);
        // prompt_tokens = 100k + 80k + 20k = 200k (at threshold, not above)
        let expected =
            (100_000.0 * 3.0 + 5_000.0 * 15.0 + 80_000.0 * 0.30 + 20_000.0 * 3.75) / 1_000_000.0;
        assert!(
            (cost - expected).abs() < 0.001,
            "cost={cost}, expected={expected}"
        );
    }

    #[test]
    fn cost_for_usage_above_threshold_uses_tiered_rates() {
        let usage = TokenUsage {
            input_tokens: 100_000,
            output_tokens: 10_000,
            cache_read_input_tokens: 90_000,
            cache_creation_input_tokens: 20_000,
        };
        let pricing = pricing_for_model("sonnet").unwrap();
        let cost = cost_for_usage(usage, pricing);
        // prompt_tokens = 100k + 90k + 20k = 210k (above 200k)
        // cache uses above-threshold rates: cache_read=0.50, cache_creation=5.00
        let expected =
            (100_000.0 * 3.0 + 10_000.0 * 15.0 + 90_000.0 * 0.50 + 20_000.0 * 5.0) / 1_000_000.0;
        assert!(
            (cost - expected).abs() < 0.001,
            "cost={cost}, expected={expected}"
        );
    }

    fn default_view() -> StatuslineView {
        StatuslineView {
            context_pct: None,
            usage: None,
            cost: None,
            model_name: "unknown".to_string(),
            branch: None,
            workspace_name: None,
            savings: None,
        }
    }

    #[test]
    fn context_bar_renders_progress_at_normal_level() {
        let view = StatuslineView {
            context_pct: Some(42),
            ..default_view()
        };
        let segment = ContextBarSegment;
        let output = segment.render(&view, false).unwrap();
        assert_eq!(output, "[ctx █████░░░░░░░ 42%]");
    }

    #[test]
    fn context_bar_renders_warning_zone() {
        let view = StatuslineView {
            context_pct: Some(75),
            ..default_view()
        };
        let segment = ContextBarSegment;
        let output = segment.render(&view, false).unwrap();
        assert_eq!(output, "[ctx █████████░░░ 75%]");
    }

    #[test]
    fn context_bar_renders_critical_at_eighty_five_percent() {
        let view = StatuslineView {
            context_pct: Some(95),
            ..default_view()
        };
        let segment = ContextBarSegment;
        let output = segment.render(&view, false).unwrap();
        assert_eq!(output, "[ctx ███████████░ 95%]");
    }

    #[test]
    fn context_bar_renders_empty_when_no_data() {
        let view = StatuslineView {
            context_pct: None,
            ..default_view()
        };
        let segment = ContextBarSegment;
        let output = segment.render(&view, false).unwrap();
        assert_eq!(output, "[ctx ░░░░░░░░░░░░ --]");
    }
}
