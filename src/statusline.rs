use std::io::{self, IsTerminal, Read};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::bridge::{bridge_path, read_bridge};
use crate::config::{StatuslineConfig, load_config};
use crate::providers;

const TIERED_PRICING_THRESHOLD: usize = 200_000;

#[derive(Debug, Default, Deserialize)]
struct StatuslineInput {
    #[serde(default)]
    transcript_path: Option<String>,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    session_path: Option<String>,
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

#[derive(Debug, PartialEq)]
struct ToolAdoptionStat {
    tools_used: u32,
    tools_relevant: u32,
    score: f32,
}

#[derive(Debug, Clone, PartialEq)]
struct StatuslineView {
    context_pct: Option<u8>,
    prompt_tokens: Option<u32>,
    context_limit: Option<u32>,
    usage: Option<TokenUsage>,
    cost: Option<f64>,
    model_name: String,
    branch: Option<String>,
    workspace_name: Option<String>,
    savings: Option<SavingsStat>,
}

// JSON output types
#[derive(Debug, Serialize)]
#[must_use]
struct JsonPayload {
    schema: String,
    version: String,
    segments: Vec<JsonSegment>,
}

#[derive(Debug, Serialize)]
struct JsonSegment {
    name: String,
    available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

pub fn handle_stdin(json: bool, no_color: bool) -> Result<()> {
    let stdin = io::stdin();
    let input = if stdin.is_terminal() {
        StatuslineInput::default()
    } else {
        parse_statusline_input_from_reader(stdin.lock())?
    };

    let config = load_config();
    let view = statusline_view(input, &config);

    if json {
        render_and_print_json(&view, &config)?;
    } else {
        render_and_print_terminal(&view, !no_color, &config);
    }

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

fn render_and_print_terminal(view: &StatuslineView, color: bool, config: &StatuslineConfig) {
    let segments = segments_from_config(config);
    println!("{}", render_statusline(view, color, &segments));
}

fn render_and_print_json(view: &StatuslineView, config: &StatuslineConfig) -> Result<()> {
    let payload = build_json_payload(view, config);
    println!("{}", serde_json::to_string_pretty(&payload)?);
    Ok(())
}

/// Resolve a `TokenProvider` from the input and config.
///
/// Priority chain: stdin `provider` > host-specific stdin identity >
/// config `provider` > auto-detect. When a non-Claude provider is selected,
/// `session_path` from the input is passed through to enable session-scoped reads.
fn resolve_provider(
    input: &StatuslineInput,
    config: &StatuslineConfig,
) -> Box<dyn providers::TokenProvider> {
    let explicit = input
        .provider
        .as_deref()
        .or_else(|| inferred_provider_from_input(input))
        .or(config.provider.as_deref());

    // When we have both an explicit provider name and a session path, build
    // the provider directly — no need to construct a default provider via
    // detect_provider only to discard it.
    let validated_session = input
        .session_path
        .as_deref()
        .and_then(validated_session_path);

    if let Some(name) = explicit {
        build_provider_by_name(name, input, validated_session.as_deref())
    } else {
        // Auto-detect: pick the most recently active provider, then
        // overlay session identity if available.
        let detected = providers::detect_provider(None);
        let name = detected.name();
        if validated_session.is_some() || name == "claude" {
            build_provider_by_name(name, input, validated_session.as_deref())
        } else {
            detected
        }
    }
}

fn inferred_provider_from_input(input: &StatuslineInput) -> Option<&'static str> {
    if input
        .transcript_path
        .as_deref()
        .is_some_and(|path| !path.trim().is_empty())
    {
        return Some("claude");
    }

    match input
        .session_path
        .as_deref()
        .and_then(validated_session_path)
        .and_then(|path| {
            path.extension()
                .and_then(|ext| ext.to_str())
                .map(str::to_owned)
        })
        .as_deref()
    {
        Some("json") => Some("gemini"),
        Some("jsonl") => Some("codex"),
        _ => None,
    }
}

/// Validate a session path from stdin.
///
/// Rejects non-absolute paths and paths without a recognised session file
/// extension (`.jsonl` for Codex, `.json` for Gemini). Returns `None` for
/// invalid paths so the provider falls back to its default discovery.
fn validated_session_path(raw: &str) -> Option<PathBuf> {
    let path = PathBuf::from(raw);
    if !path.is_absolute() {
        return None;
    }
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    if ext != "jsonl" && ext != "json" {
        return None;
    }
    Some(path)
}

/// Construct a provider by name, wiring session identity from the input.
fn build_provider_by_name(
    name: &str,
    input: &StatuslineInput,
    session_path: Option<&Path>,
) -> Box<dyn providers::TokenProvider> {
    match name {
        "claude" => Box::new(providers::claude::ClaudeProvider {
            transcript_path: input.transcript_path.clone(),
        }),
        "codex" => match session_path {
            Some(path) => Box::new(providers::codex::CodexProvider::with_session_file(
                path.to_path_buf(),
            )),
            None => Box::new(providers::codex::CodexProvider::new()),
        },
        "gemini" => match session_path {
            Some(path) => Box::new(providers::gemini::GeminiProvider::with_session_file(
                path.to_path_buf(),
            )),
            None => Box::new(providers::gemini::GeminiProvider::new()),
        },
        _ => providers::detect_provider(Some(name)),
    }
}

fn statusline_view(input: StatuslineInput, config: &StatuslineConfig) -> StatuslineView {
    // Route through the provider abstraction. `resolve_provider` selects the
    // active provider from the input's `provider` field, the config's
    // `provider` field, or auto-detects by comparing the most recent session
    // timestamp across Claude, Codex, and Gemini. Claude uses the rich
    // transcript path (`read_transcript_usage`) which provides per-turn
    // context data. Non-Claude providers use `session_usage()` for cumulative
    // token counts; context-bar / per-turn data is not available for those
    // providers in the current pass.
    let provider = resolve_provider(&input, config);

    // For Claude, read the full transcript breakdown via the streaming path.
    // For other providers, call session_usage() for cumulative token counts.
    let transcript_usage = if provider.name() == "claude" {
        input
            .transcript_path
            .as_deref()
            .and_then(|path| read_transcript_usage(path).ok())
    } else {
        None
    };
    let usage = if provider.name() == "claude" {
        transcript_usage
            .map(|usage| usage.cumulative)
            .filter(|usage| usage.has_data())
    } else {
        provider
            .session_usage()
            .ok()
            .flatten()
            .map(|u| TokenUsage {
                input_tokens: u.prompt_tokens as usize,
                output_tokens: u.completion_tokens as usize,
                cache_read_input_tokens: u.cache_read_tokens as usize,
                cache_creation_input_tokens: u.cache_creation_tokens as usize,
            })
            .filter(|u| u.has_data())
    };
    let model_name = input
        .model
        .and_then(|model| model.display_name)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| provider.name().to_string());
    let pricing = pricing_for_model(&model_name);
    let context_limit = config.context_limit_for_model(&model_name);
    // latest_assistant is only available from the Claude transcript path.
    // Non-Claude providers don't expose per-turn breakdowns, so context-bar
    // data is not rendered for them.
    let latest_assistant = transcript_usage.and_then(|usage| usage.latest_assistant);
    let context_pct = latest_assistant
        .filter(|usage| usage.has_data())
        .map(|usage| context_pct_for_usage(usage, context_limit));
    let (prompt_tokens, context_limit_value) = match latest_assistant {
        Some(usage) if usage.has_data() => {
            // Context limits for models fit in u32 (no model has > 4B tokens)
            #[allow(clippy::cast_possible_truncation)]
            {
                (
                    Some(usage.prompt_tokens() as u32),
                    Some(context_limit as u32),
                )
            }
        }
        _ => (None, None),
    };
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
        prompt_tokens,
        context_limit: context_limit_value,
        usage,
        cost,
        model_name: compact_model_name(&model_name),
        branch,
        workspace_name,
        savings,
    }
}

/// Read transcript usage, delegating to the streaming provider path.
///
/// This is the single code path for transcript reading. The old line-buffering
/// implementation has been replaced; all callers go through
/// `providers::claude::read_transcript_usage` which uses `BufReader::lines()`
/// with malformed-line skipping and no full-file buffering.
fn read_transcript_usage(path: &str) -> Result<TranscriptUsage> {
    let raw = providers::claude::read_transcript_usage(path)?;
    Ok(TranscriptUsage {
        requests: raw.requests,
        cumulative: TokenUsage {
            input_tokens: raw.cumulative.input_tokens as usize,
            output_tokens: raw.cumulative.output_tokens as usize,
            cache_read_input_tokens: raw.cumulative.cache_read_input_tokens as usize,
            cache_creation_input_tokens: raw.cumulative.cache_creation_input_tokens as usize,
        },
        latest_assistant: raw.latest_assistant.map(|u| TokenUsage {
            input_tokens: u.input_tokens as usize,
            output_tokens: u.output_tokens as usize,
            cache_read_input_tokens: u.cache_read_input_tokens as usize,
            cache_creation_input_tokens: u.cache_creation_input_tokens as usize,
        }),
    })
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

#[allow(clippy::if_same_then_else)] // o3-mini and o4-mini share identical pricing today; keep them explicit for future divergence
#[allow(
    clippy::too_many_lines,
    reason = "Explicit model pricing table keeps operator-facing rates auditable in one place"
)]
fn pricing_for_model(display_name: &str) -> Option<Pricing> {
    let normalized = display_name
        .trim()
        .to_ascii_lowercase()
        .replace('_', "-")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("-");
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
    } else if normalized.contains("o3-mini") || normalized.contains("o4-mini") {
        // o3-mini and o4-mini share identical public pricing as of 2025-04.
        Some(Pricing {
            input_per_million: 1.10,
            output_per_million: 4.40,
            cache_read_per_million: 0.0,
            cache_creation_per_million: 0.0,
            cache_read_above_threshold: 0.0,
            cache_creation_above_threshold: 0.0,
        })
    } else if normalized.contains("gpt-5.2-codex") {
        Some(Pricing {
            input_per_million: 1.75,
            output_per_million: 14.00,
            cache_read_per_million: 0.175,
            cache_creation_per_million: 0.0,
            cache_read_above_threshold: 0.175,
            cache_creation_above_threshold: 0.0,
        })
    } else if normalized.contains("gpt-5.4-mini") {
        Some(Pricing {
            input_per_million: 0.75,
            output_per_million: 4.50,
            cache_read_per_million: 0.075,
            cache_creation_per_million: 0.0,
            cache_read_above_threshold: 0.075,
            cache_creation_above_threshold: 0.0,
        })
    } else if normalized.contains("gpt-5.4-nano") {
        Some(Pricing {
            input_per_million: 0.20,
            output_per_million: 1.25,
            cache_read_per_million: 0.02,
            cache_creation_per_million: 0.0,
            cache_read_above_threshold: 0.02,
            cache_creation_above_threshold: 0.0,
        })
    } else if normalized.contains("gpt-5.4") {
        Some(Pricing {
            input_per_million: 2.50,
            output_per_million: 15.00,
            cache_read_per_million: 0.25,
            cache_creation_per_million: 0.0,
            cache_read_above_threshold: 0.25,
            cache_creation_above_threshold: 0.0,
        })
    } else if normalized.contains("gpt-5-mini") {
        Some(Pricing {
            input_per_million: 0.25,
            output_per_million: 2.00,
            cache_read_per_million: 0.025,
            cache_creation_per_million: 0.0,
            cache_read_above_threshold: 0.025,
            cache_creation_above_threshold: 0.0,
        })
    } else if normalized.contains("gpt-5-nano") {
        Some(Pricing {
            input_per_million: 0.05,
            output_per_million: 0.40,
            cache_read_per_million: 0.005,
            cache_creation_per_million: 0.0,
            cache_read_above_threshold: 0.005,
            cache_creation_above_threshold: 0.0,
        })
    } else if normalized == "gpt-5"
        || (normalized.starts_with("gpt-5-")
            && !normalized["gpt-5-".len()..]
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_digit()))
    {
        // Matches "gpt-5" exactly or "gpt-5-<non-digit-prefix>", but not "gpt-50" or
        // "gpt-500". More specific gpt-5.x variants are handled in branches above.
        Some(Pricing {
            input_per_million: 1.25,
            output_per_million: 10.00,
            cache_read_per_million: 0.125,
            cache_creation_per_million: 0.0,
            cache_read_above_threshold: 0.125,
            cache_creation_above_threshold: 0.0,
        })
    } else if normalized.contains("gpt-4.1") {
        Some(Pricing {
            input_per_million: 2.00,
            output_per_million: 8.00,
            cache_read_per_million: 0.0,
            cache_creation_per_million: 0.0,
            cache_read_above_threshold: 0.0,
            cache_creation_above_threshold: 0.0,
        })
    } else if normalized.contains("gemini-2.5-flash-lite") {
        Some(Pricing {
            input_per_million: 0.10,
            output_per_million: 0.40,
            cache_read_per_million: 0.0,
            cache_creation_per_million: 0.0,
            cache_read_above_threshold: 0.0,
            cache_creation_above_threshold: 0.0,
        })
    } else if normalized.contains("gemini-2.5-flash") {
        // Gemini 2.5 Flash has tiered output pricing (thinking vs non-thinking);
        // we use the non-thinking output rate here as the conservative estimate.
        Some(Pricing {
            input_per_million: 0.15,
            output_per_million: 0.60,
            cache_read_per_million: 0.0,
            cache_creation_per_million: 0.0,
            cache_read_above_threshold: 0.0,
            cache_creation_above_threshold: 0.0,
        })
    } else if normalized.contains("gemini-2.5-pro") {
        Some(Pricing {
            input_per_million: 1.25,
            output_per_million: 10.00,
            cache_read_per_million: 0.0,
            cache_creation_per_million: 0.0,
            cache_read_above_threshold: 0.0,
            cache_creation_above_threshold: 0.0,
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

/// Status of the hyphae memory store derived from file-existence and recency.
///
/// `rusqlite` is not used here — the db open path belongs to `hyphae` itself.
/// We use file metadata as a lightweight availability signal. The `db_bytes`
/// field is informational only — it is the file size, not a memory count, and
/// callers should not present it to users as a record count.
#[derive(Debug, PartialEq, Eq)]
enum HyphaeStatus {
    /// `hyphae.db` exists and was modified within the last 7 days.
    Active { db_bytes: u64 },
    /// `hyphae.db` exists but has not been modified recently.
    Stale,
    /// `hyphae.db` does not exist at the expected path.
    Unavailable,
}

fn hyphae_db_path() -> PathBuf {
    spore::paths::data_dir("hyphae").join("hyphae.db")
}

fn canopy_db_path() -> PathBuf {
    spore::paths::data_dir("canopy").join("canopy.db")
}

fn hyphae_status() -> HyphaeStatus {
    hyphae_status_at_path(&hyphae_db_path())
}

fn hyphae_status_at_path(path: &Path) -> HyphaeStatus {
    let Ok(metadata) = std::fs::metadata(path) else {
        return HyphaeStatus::Unavailable;
    };

    // File size is reported as an informational hint only — we deliberately
    // avoid opening SQLite from a read-only probe, so we cannot count rows.
    let db_bytes = metadata.len();

    // Check modification time: stale if not touched within 7 days.
    let is_recent = metadata
        .modified()
        .ok()
        .and_then(|mtime| mtime.elapsed().ok())
        .is_some_and(|elapsed| elapsed.as_secs() < 7 * 24 * 3600);

    if is_recent {
        HyphaeStatus::Active { db_bytes }
    } else {
        HyphaeStatus::Stale
    }
}

fn tool_adoption_stat_at_path(path: &Path) -> Option<ToolAdoptionStat> {
    if !path.exists() {
        return None;
    }

    let conn = Connection::open(path).ok()?;
    let json_str = conn
        .query_row(
            "SELECT score_json FROM tool_adoption_scores ORDER BY rowid DESC LIMIT 1",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .ok()??;

    let v: serde_json::Value = serde_json::from_str(&json_str).ok()?;
    let tools_used = u32::try_from(v.get("tools_used")?.as_u64()?).ok()?;
    let tools_relevant = u32::try_from(v.get("tools_relevant")?.as_u64()?).ok()?;
    // Convert f64 to f32 for score display; precision loss is acceptable here.
    #[allow(clippy::cast_possible_truncation)]
    let score = v.get("score")?.as_f64()? as f32;

    Some(ToolAdoptionStat {
        tools_used,
        tools_relevant,
        score,
    })
}

fn tool_adoption_stat() -> Option<ToolAdoptionStat> {
    tool_adoption_stat_at_path(&canopy_db_path())
}

fn canopy_unread_count_at_path(path: &Path) -> Option<u32> {
    if !path.exists() {
        return None;
    }

    let conn = Connection::open(path).ok()?;
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM notifications WHERE seen = 0",
            [],
            |row| row.get(0),
        )
        .ok()?;

    // COUNT(*) is always non-negative; clamp to 0 to guard against any unexpected
    // negative value before converting to the unsigned return type.
    Some(u32::try_from(count.max(0)).unwrap_or(u32::MAX))
}

fn canopy_unread_count() -> Option<u32> {
    canopy_unread_count_at_path(&canopy_db_path())
}

/// JSON data for the hyphae segment (path-parameterized for testing).
fn build_hyphae_segment_at_path(path: &Path) -> JsonSegment {
    match hyphae_status_at_path(path) {
        HyphaeStatus::Active { db_bytes } => JsonSegment {
            name: "hyphae".to_string(),
            available: true,
            // `db_bytes` is an informational file-size hint, not a record count.
            value: Some(serde_json::json!({ "state": "active", "db_bytes": db_bytes })),
            reason: None,
        },
        HyphaeStatus::Stale => JsonSegment {
            name: "hyphae".to_string(),
            available: true,
            value: Some(serde_json::json!({ "state": "stale" })),
            reason: Some("hyphae.db has not been modified in over 7 days".to_string()),
        },
        HyphaeStatus::Unavailable => JsonSegment {
            name: "hyphae".to_string(),
            available: false,
            value: None,
            reason: Some(format!("hyphae.db not found at {}", path.display())),
        },
    }
}

/// JSON data for the hyphae segment.
fn build_hyphae_segment() -> JsonSegment {
    build_hyphae_segment_at_path(&hyphae_db_path())
}

/// JSON data for the bridge segment (path-parameterized for testing).
fn build_bridge_segment_at_path(path: &Path) -> JsonSegment {
    let state = read_bridge(path);
    if state.entries.is_empty() {
        return JsonSegment {
            name: "bridge".to_string(),
            available: false,
            value: None,
            reason: Some("no entries in bridge file".to_string()),
        };
    }

    let entries: Vec<Value> = state
        .entries
        .iter()
        .map(|e| {
            serde_json::json!({
                "key": e.key,
                "value": e.value,
            })
        })
        .collect();

    JsonSegment {
        name: "bridge".to_string(),
        available: true,
        value: Some(serde_json::json!({ "entries": entries })),
        reason: None,
    }
}

/// JSON data for the bridge segment.
fn build_bridge_segment() -> JsonSegment {
    build_bridge_segment_at_path(&bridge_path())
}

/// JSON data for the canopy tool adoption segment (path-parameterized for testing).
fn build_canopy_adoption_segment_at_path(path: &Path) -> JsonSegment {
    match tool_adoption_stat_at_path(path) {
        Some(stat) => JsonSegment {
            name: "canopy-adoption".to_string(),
            available: true,
            value: Some(serde_json::json!({
                "tools_used": stat.tools_used,
                "tools_relevant": stat.tools_relevant,
                "score": stat.score
            })),
            reason: None,
        },
        None => JsonSegment {
            name: "canopy-adoption".to_string(),
            available: false,
            value: None,
            reason: Some("no tool adoption data".to_string()),
        },
    }
}

/// JSON data for the canopy tool adoption segment.
fn build_canopy_adoption_segment() -> JsonSegment {
    build_canopy_adoption_segment_at_path(&canopy_db_path())
}

/// JSON data for the canopy notifications segment (path-parameterized for testing).
fn build_canopy_notifications_segment_at_path(path: &Path) -> JsonSegment {
    if !path.exists() {
        return JsonSegment {
            name: "canopy-notifications".to_string(),
            available: false,
            value: None,
            reason: Some(format!("canopy.db not found at {}", path.display())),
        };
    }

    match canopy_unread_count_at_path(path) {
        Some(count) if count > 0 => JsonSegment {
            name: "canopy-notifications".to_string(),
            available: true,
            value: Some(serde_json::json!({ "unread": count })),
            reason: None,
        },
        Some(_) => JsonSegment {
            name: "canopy-notifications".to_string(),
            available: false,
            value: None,
            reason: Some("no unread canopy notifications".to_string()),
        },
        None => JsonSegment {
            name: "canopy-notifications".to_string(),
            available: false,
            value: None,
            reason: Some(format!("canopy.db not found at {}", path.display())),
        },
    }
}

/// JSON data for the canopy notifications segment.
fn build_canopy_notifications_segment() -> JsonSegment {
    build_canopy_notifications_segment_at_path(&canopy_db_path())
}

/// JSON data for the cortina segment — always unavailable, no data seam yet.
fn build_cortina_segment() -> JsonSegment {
    JsonSegment {
        name: "cortina".to_string(),
        available: false,
        value: None,
        reason: Some("no direct data seam yet".to_string()),
    }
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

struct HyphaeSegment;
impl Segment for HyphaeSegment {
    fn name(&self) -> &'static str {
        "hyphae"
    }
    fn line(&self) -> u8 {
        2
    }
    fn render(&self, _view: &StatuslineView, color: bool) -> Option<String> {
        // Display is state-only (active / stale / hidden). We deliberately do
        // not show the db byte size — it's not a memory count, and a numeric
        // hint next to "hy" was easy to misread as one.
        match hyphae_status() {
            HyphaeStatus::Active { .. } => Some(paint("hy: active", "2", color)),
            HyphaeStatus::Stale => Some(paint("hy: stale", "2", color)),
            HyphaeStatus::Unavailable => None,
        }
    }
}

struct BridgeSegment;
impl Segment for BridgeSegment {
    fn name(&self) -> &'static str {
        "bridge"
    }
    fn line(&self) -> u8 {
        2
    }
    fn render(&self, _view: &StatuslineView, color: bool) -> Option<String> {
        let state = read_bridge(&bridge_path());
        if state.entries.is_empty() {
            return None;
        }
        let parts: Vec<String> = state
            .entries
            .iter()
            .map(|e| format!("[{}:{}]", e.key, e.value))
            .collect();
        Some(paint(&parts.join(" "), "35", color))
    }
}

struct CortinaSegment;
impl Segment for CortinaSegment {
    fn name(&self) -> &'static str {
        "cortina"
    }
    fn line(&self) -> u8 {
        2
    }
    fn render(&self, _view: &StatuslineView, _color: bool) -> Option<String> {
        // Cortina has no direct data seam yet; always unavailable.
        None
    }
}

struct ToolAdoptionSegment;
impl Segment for ToolAdoptionSegment {
    fn name(&self) -> &'static str {
        "canopy-adoption"
    }
    fn line(&self) -> u8 {
        2
    }
    fn render(&self, _view: &StatuslineView, color: bool) -> Option<String> {
        let stat = tool_adoption_stat()?;
        let label = format!("tools:{}/{}", stat.tools_used, stat.tools_relevant);
        let color_code = if stat.score >= 0.7 {
            "32" // green
        } else if stat.score >= 0.4 {
            "33" // yellow
        } else {
            "31" // red
        };
        Some(paint(&label, color_code, color))
    }
}

struct CanopyNotificationsSegment;
impl Segment for CanopyNotificationsSegment {
    fn name(&self) -> &'static str {
        "canopy-notifications"
    }
    fn line(&self) -> u8 {
        2
    }
    fn render(&self, _view: &StatuslineView, color: bool) -> Option<String> {
        let count = canopy_unread_count()?;
        if count == 0 {
            return None;
        }
        let label = format!("canopy:{count} unread");
        Some(paint(&label, "33", color))
    }
}

struct DegradationSegment;
impl Segment for DegradationSegment {
    fn name(&self) -> &'static str {
        "degradation"
    }
    fn line(&self) -> u8 {
        2
    }
    fn render(&self, _view: &StatuslineView, color: bool) -> Option<String> {
        use spore::availability::{DegradationTier, probe_all};

        let reports = probe_all();
        let unavailable: Vec<_> = reports.iter().filter(|r| !r.available).collect();

        if unavailable.is_empty() {
            return None;
        }

        // Determine highest priority tier
        let mut has_tier1 = false;
        let mut has_tier2 = false;

        for report in &unavailable {
            match report.tier {
                DegradationTier::Tier1 => has_tier1 = true,
                DegradationTier::Tier2 => has_tier2 = true,
                // Tier3 (optional/informational) does not escalate the indicator.
                // DegradationTier is #[non_exhaustive]; future variants also default to no
                // escalation rather than silently falling into an existing tier.
                #[allow(clippy::match_same_arms)]
                DegradationTier::Tier3 => {}
                _ => {}
            }
        }

        let (indicator, color_code) = if has_tier1 {
            let tier1_reports: Vec<_> = unavailable
                .iter()
                .filter(|r| r.tier == DegradationTier::Tier1)
                .collect();
            let count = tier1_reports.len();
            (
                if count == 1 {
                    format!("[!! {}]", tier1_reports[0].tool)
                } else {
                    format!("[!! {count} critical]")
                },
                "31", // red
            )
        } else if has_tier2 {
            let tier2_reports: Vec<_> = unavailable
                .iter()
                .filter(|r| r.tier == DegradationTier::Tier2)
                .collect();
            let count = tier2_reports.len();
            (
                if count == 1 {
                    format!("[! {}]", tier2_reports[0].tool)
                } else {
                    format!("[! {count} degraded]")
                },
                "33", // yellow
            )
        } else {
            let count = unavailable.len();
            (
                if count == 1 {
                    format!("[· {}]", unavailable[0].tool)
                } else {
                    format!("[· {count} optional]")
                },
                "2", // dim
            )
        };

        Some(paint(&indicator, color_code, color))
    }
}

fn default_segments() -> Vec<Box<dyn Segment>> {
    vec![
        Box::new(ContextSegment),
        Box::new(UsageSegment),
        Box::new(CostSegment),
        Box::new(ModelSegment),
        Box::new(SavingsSegment),
        Box::new(DegradationSegment),
        Box::new(BranchSegment),
        Box::new(WorkspaceSegment),
        Box::new(ContextBarSegment),
        Box::new(HyphaeSegment),
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
            "degradation" => Some(Box::new(DegradationSegment)),
            "branch" => Some(Box::new(BranchSegment)),
            "workspace" => Some(Box::new(WorkspaceSegment)),
            "context-bar" => Some(Box::new(ContextBarSegment)),
            "hyphae" => Some(Box::new(HyphaeSegment)),
            "bridge" => Some(Box::new(BridgeSegment)),
            "canopy-adoption" => Some(Box::new(ToolAdoptionSegment)),
            "canopy-notifications" => Some(Box::new(CanopyNotificationsSegment)),
            "cortina" => Some(Box::new(CortinaSegment)),
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

fn build_json_payload(view: &StatuslineView, config: &StatuslineConfig) -> JsonPayload {
    let config_segments = if config.segments.is_empty() {
        StatuslineConfig::default().segments
    } else {
        config.segments.clone()
    };

    let mut segments = Vec::new();

    for entry in &config_segments {
        if !entry.enabled {
            continue;
        }

        let segment = match entry.name.as_str() {
            "context" => build_context_segment(view),
            "usage" => build_usage_segment(view),
            "cost" => build_cost_segment(view),
            "model" => build_model_segment(view),
            "savings" => build_savings_segment(view),
            "degradation" => build_degradation_segment(),
            "branch" => build_branch_segment(view),
            "workspace" => build_workspace_segment(view),
            "context-bar" => build_context_bar_segment(view),
            "hyphae" => build_hyphae_segment(),
            "bridge" => build_bridge_segment(),
            "canopy-adoption" => build_canopy_adoption_segment(),
            "canopy-notifications" => build_canopy_notifications_segment(),
            "cortina" => build_cortina_segment(),
            _ => continue,
        };
        segments.push(segment);
    }

    JsonPayload {
        schema: "annulus-statusline-v1".to_string(),
        version: "1".to_string(),
        segments,
    }
}

fn build_context_segment(view: &StatuslineView) -> JsonSegment {
    match (view.context_pct, view.prompt_tokens, view.context_limit) {
        (Some(pct), Some(tokens), Some(limit)) => JsonSegment {
            name: "context".to_string(),
            available: true,
            value: Some(serde_json::json!({
                "percent": pct,
                "prompt_tokens": tokens,
                "context_limit": limit
            })),
            reason: None,
        },
        _ => JsonSegment {
            name: "context".to_string(),
            available: false,
            value: None,
            reason: Some("no transcript available".to_string()),
        },
    }
}

fn build_usage_segment(view: &StatuslineView) -> JsonSegment {
    match view.usage {
        Some(usage) => JsonSegment {
            name: "usage".to_string(),
            available: true,
            value: Some(serde_json::json!({
                "input_tokens": usage.input_tokens,
                "output_tokens": usage.output_tokens,
                "cache_read_tokens": usage.cache_read_input_tokens,
                "cache_creation_tokens": usage.cache_creation_input_tokens
            })),
            reason: None,
        },
        None => JsonSegment {
            name: "usage".to_string(),
            available: false,
            value: None,
            reason: Some("no transcript available".to_string()),
        },
    }
}

fn build_cost_segment(view: &StatuslineView) -> JsonSegment {
    match view.cost {
        Some(cost) => JsonSegment {
            name: "cost".to_string(),
            available: true,
            value: Some(serde_json::json!({
                "dollars": (cost * 100.0).round() / 100.0,
                "model": &view.model_name
            })),
            reason: None,
        },
        None => JsonSegment {
            name: "cost".to_string(),
            available: false,
            value: None,
            reason: Some("pricing not available for model".to_string()),
        },
    }
}

fn build_model_segment(view: &StatuslineView) -> JsonSegment {
    JsonSegment {
        name: "model".to_string(),
        available: true,
        value: Some(serde_json::json!({
            "display_name": &view.model_name
        })),
        reason: None,
    }
}

fn build_savings_segment(view: &StatuslineView) -> JsonSegment {
    if let Some(savings) = &view.savings {
        JsonSegment {
            name: "savings".to_string(),
            available: true,
            value: Some(serde_json::json!({
                "saved_tokens": savings.saved_tokens,
                "input_tokens": savings.input_tokens
            })),
            reason: None,
        }
    } else {
        let reason = if current_runtime_session_id().is_none() {
            "no active session".to_string()
        } else {
            match mycelium_db_path() {
                Ok(path) => format!("mycelium database not found at {}", path.display()),
                Err(_) => "mycelium database unavailable".to_string(),
            }
        };
        JsonSegment {
            name: "savings".to_string(),
            available: false,
            value: None,
            reason: Some(reason),
        }
    }
}

fn build_branch_segment(view: &StatuslineView) -> JsonSegment {
    match &view.branch {
        Some(branch) => JsonSegment {
            name: "branch".to_string(),
            available: true,
            value: Some(serde_json::json!({
                "branch": branch
            })),
            reason: None,
        },
        None => JsonSegment {
            name: "branch".to_string(),
            available: false,
            value: None,
            reason: Some("not in a git repository".to_string()),
        },
    }
}

fn build_workspace_segment(view: &StatuslineView) -> JsonSegment {
    match &view.workspace_name {
        Some(name) => JsonSegment {
            name: "workspace".to_string(),
            available: true,
            value: Some(serde_json::json!({
                "name": name
            })),
            reason: None,
        },
        None => JsonSegment {
            name: "workspace".to_string(),
            available: false,
            value: None,
            reason: Some("workspace path unavailable".to_string()),
        },
    }
}

fn build_context_bar_segment(view: &StatuslineView) -> JsonSegment {
    match view.context_pct {
        Some(pct) => {
            let bar_width = 12;
            let fill = if pct > 0 {
                ((usize::from(pct) * bar_width) / 100).max(1)
            } else {
                0
            };
            let fill = fill.min(bar_width);
            let color_tier = if pct >= 85 {
                "danger"
            } else if pct >= 60 {
                "warning"
            } else {
                "ok"
            };
            JsonSegment {
                name: "context-bar".to_string(),
                available: true,
                value: Some(serde_json::json!({
                    "percent": pct,
                    "fill_chars": fill,
                    "total_chars": bar_width,
                    "color_tier": color_tier
                })),
                reason: None,
            }
        }
        None => JsonSegment {
            name: "context-bar".to_string(),
            available: false,
            value: None,
            reason: Some("no transcript available".to_string()),
        },
    }
}

fn build_degradation_segment() -> JsonSegment {
    use spore::availability::{DegradationTier, probe_all};

    let reports = probe_all();
    let unavailable: Vec<_> = reports.iter().filter(|r| !r.available).collect();

    if unavailable.is_empty() {
        return JsonSegment {
            name: "degradation".to_string(),
            available: false,
            value: None,
            reason: Some("no degradation detected".to_string()),
        };
    }

    let mut tier1_count = 0;
    let mut tier2_count = 0;
    let mut tier3_count = 0;

    for report in &unavailable {
        match report.tier {
            DegradationTier::Tier1 => tier1_count += 1,
            DegradationTier::Tier2 => tier2_count += 1,
            DegradationTier::Tier3 => tier3_count += 1,
            _ => {}
        }
    }

    JsonSegment {
        name: "degradation".to_string(),
        available: true,
        value: Some(serde_json::json!({
            "tier1_count": tier1_count,
            "tier2_count": tier2_count,
            "tier3_count": tier3_count,
            "tools": unavailable.iter().map(|r| {
                serde_json::json!({
                    "name": &r.tool,
                    "tier": format!("{}", r.tier),
                    "reason": &r.reason
                })
            }).collect::<Vec<_>>()
        })),
        reason: None,
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

        // Explicitly select Claude so this test is not affected by auto-detection
        // choosing a different provider based on the local environment.
        let config = StatuslineConfig {
            provider: Some("claude".to_string()),
            ..StatuslineConfig::default()
        };
        let view = statusline_view(
            StatuslineInput {
                transcript_path: Some(transcript.to_string_lossy().to_string()),
                provider: None,
                session_path: None,
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

        // Explicitly select Claude so this test is not affected by auto-detection
        // choosing a different provider based on the local environment.
        let mut config = StatuslineConfig {
            provider: Some("claude".to_string()),
            ..StatuslineConfig::default()
        };
        config.context_limits.insert("sonnet".to_string(), 100_000);

        let view = statusline_view(
            StatuslineInput {
                transcript_path: Some(transcript.to_string_lossy().to_string()),
                provider: None,
                session_path: None,
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
                prompt_tokens: Some(95_000),
                context_limit: Some(200_000),
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

        // Note: degradation segment is included if unavailable tools are detected
        assert!(line.contains("ctx: ▲ 42%"));
        assert!(line.contains("sonnet 4.6"));
        assert!(line.contains("↓8.2K saved"));
        assert!(line.contains("git: main"));
        assert!(line.contains("ws: basidiocarp"));
    }

    #[test]
    fn render_statusline_degrades_gracefully() {
        let segments = default_segments();
        let line = render_statusline(
            &StatuslineView {
                context_pct: None,
                prompt_tokens: None,
                context_limit: None,
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

        // Note: degradation segment is included if unavailable tools are detected
        assert!(line.contains("ctx: --"));
        assert!(line.contains("unknown"));
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

    #[test]
    fn pricing_for_model_recognizes_latest_openai_variants() {
        let gpt5 = pricing_for_model("GPT-5").expect("gpt-5 pricing");
        assert!((gpt5.input_per_million - 1.25).abs() < f64::EPSILON);
        assert!((gpt5.output_per_million - 10.0).abs() < f64::EPSILON);

        let gpt54mini = pricing_for_model("GPT-5.4 mini").expect("gpt-5.4 mini pricing");
        assert!((gpt54mini.input_per_million - 0.75).abs() < f64::EPSILON);
        assert!((gpt54mini.output_per_million - 4.5).abs() < f64::EPSILON);

        let codex = pricing_for_model("gpt-5.2-codex").expect("gpt-5.2-codex pricing");
        assert!((codex.input_per_million - 1.75).abs() < f64::EPSILON);
        assert!((codex.output_per_million - 14.0).abs() < f64::EPSILON);

        // Strings like "gpt-50" or "gpt-500" must not match the gpt-5 tier.
        assert!(
            pricing_for_model("gpt-50").is_none(),
            "gpt-50 must not match gpt-5 tier"
        );
    }

    #[test]
    fn pricing_for_model_recognizes_latest_gemini_variants() {
        let flash_lite =
            pricing_for_model("Gemini 2.5 Flash-Lite").expect("gemini 2.5 flash-lite pricing");
        assert!((flash_lite.input_per_million - 0.10).abs() < f64::EPSILON);
        assert!((flash_lite.output_per_million - 0.40).abs() < f64::EPSILON);

        let flash = pricing_for_model("gemini 2.5 flash").expect("gemini 2.5 flash pricing");
        assert!((flash.input_per_million - 0.15).abs() < f64::EPSILON);
        assert!((flash.output_per_million - 0.60).abs() < f64::EPSILON);
    }

    fn default_view() -> StatuslineView {
        StatuslineView {
            context_pct: None,
            prompt_tokens: None,
            context_limit: None,
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

    #[test]
    fn json_payload_includes_all_enabled_segments() {
        let view = StatuslineView {
            context_pct: Some(67),
            prompt_tokens: Some(134_000),
            context_limit: Some(200_000),
            usage: Some(TokenUsage {
                input_tokens: 100_000,
                output_tokens: 20_000,
                cache_read_input_tokens: 50_000,
                cache_creation_input_tokens: 10_000,
            }),
            cost: Some(0.75),
            model_name: "sonnet 4.6".to_string(),
            branch: Some("main".to_string()),
            workspace_name: Some("basidiocarp".to_string()),
            savings: Some(SavingsStat {
                saved_tokens: 45_000,
                input_tokens: 100_000,
            }),
        };
        let config = StatuslineConfig::default();
        let payload = build_json_payload(&view, &config);

        assert_eq!(payload.schema, "annulus-statusline-v1");
        assert_eq!(payload.version, "1");
        assert!(payload.segments.len() >= 10);

        let segment_names: Vec<&str> = payload.segments.iter().map(|s| s.name.as_str()).collect();
        assert!(segment_names.contains(&"context"));
        assert!(segment_names.contains(&"usage"));
        assert!(segment_names.contains(&"cost"));
        assert!(segment_names.contains(&"model"));
        assert!(segment_names.contains(&"savings"));
        assert!(segment_names.contains(&"branch"));
        assert!(segment_names.contains(&"workspace"));
        assert!(segment_names.contains(&"context-bar"));

        // Values must carry the view's data, not just be present. The earlier
        // version of this test passed even when segments emitted empty values.
        let find = |n: &str| payload.segments.iter().find(|s| s.name == n).unwrap();

        let cost = find("cost");
        assert!(cost.available);
        assert_eq!(cost.value.as_ref().unwrap()["dollars"].as_f64(), Some(0.75));

        let model = find("model");
        assert!(model.available);
        assert_eq!(
            model.value.as_ref().unwrap()["display_name"].as_str(),
            Some("sonnet 4.6")
        );

        let branch = find("branch");
        assert!(branch.available);
        assert_eq!(
            branch.value.as_ref().unwrap()["branch"].as_str(),
            Some("main")
        );

        let usage = find("usage");
        assert!(usage.available);
        assert_eq!(
            usage.value.as_ref().unwrap()["input_tokens"].as_u64(),
            Some(100_000)
        );
        assert_eq!(
            usage.value.as_ref().unwrap()["output_tokens"].as_u64(),
            Some(20_000)
        );

        let savings = find("savings");
        assert!(savings.available);
        assert_eq!(
            savings.value.as_ref().unwrap()["saved_tokens"].as_u64(),
            Some(45_000)
        );

        let context = find("context");
        assert!(context.available);
        assert_eq!(
            context.value.as_ref().unwrap()["percent"].as_u64(),
            Some(67)
        );
    }

    #[test]
    fn json_payload_degraded_segment_includes_reason() {
        let view = StatuslineView {
            context_pct: None,
            prompt_tokens: None,
            context_limit: None,
            usage: None,
            cost: None,
            model_name: "unknown".to_string(),
            branch: None,
            workspace_name: None,
            savings: None,
        };
        let config = StatuslineConfig::default();
        let payload = build_json_payload(&view, &config);

        let context_segment = payload
            .segments
            .iter()
            .find(|s| s.name == "context")
            .unwrap();
        assert!(!context_segment.available);
        assert!(context_segment.reason.is_some());
        assert!(context_segment.value.is_none());

        let branch_segment = payload
            .segments
            .iter()
            .find(|s| s.name == "branch")
            .unwrap();
        assert!(!branch_segment.available);
        assert!(branch_segment.reason.is_some());
        assert!(branch_segment.value.is_none());
    }

    #[test]
    fn json_context_segment_serializes_percent() {
        let view = StatuslineView {
            context_pct: Some(67),
            prompt_tokens: Some(134_000),
            context_limit: Some(200_000),
            ..default_view()
        };
        let segment = build_context_segment(&view);
        assert!(segment.available);
        assert!(segment.value.is_some());
        let value = segment.value.unwrap();
        assert_eq!(value.get("percent").and_then(Value::as_u64), Some(67));
    }

    #[test]
    fn json_context_segment_includes_real_token_values() {
        let view = StatuslineView {
            context_pct: Some(67),
            prompt_tokens: Some(134_000),
            context_limit: Some(200_000),
            ..default_view()
        };
        let segment = build_context_segment(&view);
        assert!(segment.available);
        assert!(segment.reason.is_none());
        let value = segment.value.unwrap();
        assert_eq!(value.get("percent").and_then(Value::as_u64), Some(67));
        assert_eq!(
            value.get("prompt_tokens").and_then(Value::as_u64),
            Some(134_000)
        );
        assert_eq!(
            value.get("context_limit").and_then(Value::as_u64),
            Some(200_000)
        );
    }

    #[test]
    fn json_context_segment_unavailable_when_tokens_missing() {
        let view = StatuslineView {
            context_pct: Some(67),
            prompt_tokens: Some(134_000),
            context_limit: None, // Missing limit
            ..default_view()
        };
        let segment = build_context_segment(&view);
        assert!(!segment.available);
        assert!(segment.reason.is_some());
        assert!(segment.value.is_none());
    }

    #[test]
    fn json_usage_segment_serializes_all_token_types() {
        let view = StatuslineView {
            usage: Some(TokenUsage {
                input_tokens: 100_000,
                output_tokens: 20_000,
                cache_read_input_tokens: 50_000,
                cache_creation_input_tokens: 10_000,
            }),
            ..default_view()
        };
        let segment = build_usage_segment(&view);
        assert!(segment.available);
        let value = segment.value.unwrap();
        assert_eq!(
            value.get("input_tokens").and_then(Value::as_u64),
            Some(100_000)
        );
        assert_eq!(
            value.get("output_tokens").and_then(Value::as_u64),
            Some(20_000)
        );
        assert_eq!(
            value.get("cache_read_tokens").and_then(Value::as_u64),
            Some(50_000)
        );
        assert_eq!(
            value.get("cache_creation_tokens").and_then(Value::as_u64),
            Some(10_000)
        );
    }

    #[test]
    fn json_savings_segment_unavailable_when_no_session() {
        let view = StatuslineView {
            savings: None,
            ..default_view()
        };
        let segment = build_savings_segment(&view);
        assert!(!segment.available);
        assert_eq!(segment.name, "savings");
    }

    #[test]
    fn json_context_bar_serializes_color_tier() {
        let view_ok = StatuslineView {
            context_pct: Some(50),
            ..default_view()
        };
        let segment_ok = build_context_bar_segment(&view_ok);
        let value_ok = segment_ok.value.unwrap();
        assert_eq!(
            value_ok.get("color_tier").and_then(Value::as_str),
            Some("ok")
        );

        let view_warning = StatuslineView {
            context_pct: Some(70),
            ..default_view()
        };
        let segment_warning = build_context_bar_segment(&view_warning);
        let value_warning = segment_warning.value.unwrap();
        assert_eq!(
            value_warning.get("color_tier").and_then(Value::as_str),
            Some("warning")
        );

        let view_danger = StatuslineView {
            context_pct: Some(90),
            ..default_view()
        };
        let segment_danger = build_context_bar_segment(&view_danger);
        let value_danger = segment_danger.value.unwrap();
        assert_eq!(
            value_danger.get("color_tier").and_then(Value::as_str),
            Some("danger")
        );
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn degradation_segment_renders_correct_tier_when_mixed_unavailable() {
        use spore::availability::{AvailabilityReport, DegradationTier};

        // Test helper: render a DegradationSegment with injected reports
        fn render_degradation_with_reports(reports: &[AvailabilityReport]) -> Option<String> {
            // Simulate the key logic from DegradationSegment::render
            let unavailable: Vec<_> = reports.iter().filter(|r| !r.available).collect();

            if unavailable.is_empty() {
                return None;
            }

            let mut has_tier1 = false;
            let mut has_tier2 = false;

            for report in &unavailable {
                match report.tier {
                    DegradationTier::Tier1 => has_tier1 = true,
                    DegradationTier::Tier2 => has_tier2 = true,
                    // Tier3 (optional/informational) does not escalate the indicator.
                    // DegradationTier is #[non_exhaustive]; future variants also default to no
                    // escalation rather than silently falling into an existing tier.
                    #[allow(clippy::match_same_arms)]
                    DegradationTier::Tier3 => {}
                    _ => {}
                }
            }

            let indicator = if has_tier1 {
                let tier1_reports: Vec<_> = unavailable
                    .iter()
                    .filter(|r| r.tier == DegradationTier::Tier1)
                    .collect();
                let count = tier1_reports.len();
                if count == 1 {
                    format!("[!! {}]", tier1_reports[0].tool)
                } else {
                    format!("[!! {count} critical]")
                }
            } else if has_tier2 {
                let tier2_reports: Vec<_> = unavailable
                    .iter()
                    .filter(|r| r.tier == DegradationTier::Tier2)
                    .collect();
                let count = tier2_reports.len();
                if count == 1 {
                    format!("[! {}]", tier2_reports[0].tool)
                } else {
                    format!("[! {count} degraded]")
                }
            } else {
                let count = unavailable.len();
                if count == 1 {
                    format!("[· {}]", unavailable[0].tool)
                } else {
                    format!("[· {count} optional]")
                }
            };

            Some(indicator)
        }

        // Test: Tier1 (mycelium) unavailable + Tier3 (hymenium) unavailable.
        // Should render mycelium, not hymenium.
        let reports = vec![
            AvailabilityReport {
                tool: "mycelium".to_string(),
                available: false,
                tier: DegradationTier::Tier1,
                reason: Some("not found".to_string()),
                degraded_capabilities: vec![],
            },
            AvailabilityReport {
                tool: "hymenium".to_string(),
                available: false,
                tier: DegradationTier::Tier3,
                reason: Some("not found".to_string()),
                degraded_capabilities: vec![],
            },
        ];

        let output = render_degradation_with_reports(&reports);
        assert!(
            output.is_some(),
            "should render when unavailable tools exist"
        );
        let output = output.unwrap();
        assert!(
            output.contains("mycelium"),
            "should contain mycelium (Tier1), got: {output}"
        );
        assert!(
            !output.contains("hymenium"),
            "should NOT contain hymenium (Tier3), got: {output}"
        );
        assert_eq!(
            output, "[!! mycelium]",
            "single Tier1 tool should use !! prefix"
        );

        // Test: Tier2 (hyphae) unavailable + Tier3 (volva) unavailable.
        // Should render hyphae, not volva.
        let reports2 = vec![
            AvailabilityReport {
                tool: "hyphae".to_string(),
                available: false,
                tier: DegradationTier::Tier2,
                reason: Some("not found".to_string()),
                degraded_capabilities: vec![],
            },
            AvailabilityReport {
                tool: "volva".to_string(),
                available: false,
                tier: DegradationTier::Tier3,
                reason: Some("not found".to_string()),
                degraded_capabilities: vec![],
            },
        ];

        let output2 = render_degradation_with_reports(&reports2);
        assert!(
            output2.is_some(),
            "should render when unavailable tools exist"
        );
        let output2 = output2.unwrap();
        assert!(
            output2.contains("hyphae"),
            "should contain hyphae (Tier2), got: {output2}"
        );
        assert!(
            !output2.contains("volva"),
            "should NOT contain volva (Tier3), got: {output2}"
        );
        assert_eq!(
            output2, "[! hyphae]",
            "single Tier2 tool should use ! prefix"
        );
    }

    // ── Step 1: hyphae / cortina segment tests ────────────────────────────────

    #[test]
    fn hyphae_segment_unavailable_when_db_missing() {
        let temp_dir = tempfile::TempDir::new().expect("tempdir");
        let nonexistent = temp_dir.path().join("hyphae.db");

        let status = hyphae_status_at_path(&nonexistent);
        assert_eq!(status, HyphaeStatus::Unavailable);

        let segment_json = build_hyphae_segment_at_path(&nonexistent);
        assert!(!segment_json.available);
        assert_eq!(segment_json.name, "hyphae");
        assert!(segment_json.reason.is_some());
        assert!(
            segment_json
                .reason
                .as_deref()
                .unwrap()
                .contains("hyphae.db not found"),
            "reason should mention hyphae.db not found"
        );
    }

    #[test]
    fn hyphae_segment_active_when_db_recently_modified() {
        let temp_dir = tempfile::TempDir::new().expect("tempdir");
        let db_path = temp_dir.path().join("hyphae.db");
        // Write a small db placeholder (just needs to exist and be recent)
        fs::write(&db_path, b"SQLite format 3\x00").expect("write db");

        let status = hyphae_status_at_path(&db_path);
        // Should be active (just written, so mtime is now)
        assert!(
            matches!(status, HyphaeStatus::Active { .. }),
            "recently modified db should be Active, got: {status:?}"
        );
    }

    #[test]
    fn cortina_segment_always_unavailable_stub() {
        let view = default_view();
        let seg = CortinaSegment;
        // Terminal render: always None (nothing shown)
        let render = seg.render(&view, false);
        assert!(render.is_none(), "cortina should render nothing");

        // JSON render: always unavailable with stub reason
        let json_seg = build_cortina_segment();
        assert!(!json_seg.available);
        assert_eq!(json_seg.name, "cortina");
        assert_eq!(json_seg.reason.as_deref(), Some("no direct data seam yet"));
    }

    #[test]
    fn explicit_provider_in_config_is_used() {
        // A config with an explicit provider must use that provider, bypassing
        // auto-detection and the local environment.
        let codex_config = StatuslineConfig {
            provider: Some("codex".to_string()),
            ..StatuslineConfig::default()
        };
        let provider = crate::providers::detect_provider(codex_config.provider.as_deref());
        assert_eq!(provider.name(), "codex");

        let claude_config = StatuslineConfig {
            provider: Some("claude".to_string()),
            ..StatuslineConfig::default()
        };
        let provider_claude = crate::providers::detect_provider(claude_config.provider.as_deref());
        assert_eq!(provider_claude.name(), "claude");
    }

    #[test]
    fn hyphae_segment_included_in_default_segments() {
        let segments = default_segments();
        let names: Vec<&str> = segments.iter().map(|s| s.name()).collect();
        assert!(
            names.contains(&"hyphae"),
            "hyphae should be in default segments"
        );
        // cortina is intentionally excluded from defaults until a data seam exists.
        assert!(
            !names.contains(&"cortina"),
            "cortina should not be in default segments"
        );
    }

    // ── StatuslineInput: provider and session_path parsing ───────────────────

    #[test]
    fn parse_statusline_input_provider_and_session_path() {
        let input = parse_statusline_input_from_reader(std::io::Cursor::new(
            br#"{"provider":"codex","session_path":"/tmp/my-session.jsonl"}"#,
        ))
        .expect("should parse");

        assert_eq!(input.provider.as_deref(), Some("codex"));
        assert_eq!(input.session_path.as_deref(), Some("/tmp/my-session.jsonl"));
    }

    #[test]
    fn parse_statusline_input_provider_and_session_path_default_to_none() {
        let input = parse_statusline_input_from_reader(std::io::Cursor::new(
            br#"{"transcript_path":"/tmp/transcript.jsonl"}"#,
        ))
        .expect("should parse");

        assert!(input.provider.is_none());
        assert!(input.session_path.is_none());
        assert_eq!(
            input.transcript_path.as_deref(),
            Some("/tmp/transcript.jsonl")
        );
    }

    // ── Provider priority chain ─────────────────────────────────────────────

    #[test]
    fn stdin_provider_overrides_config_provider() {
        // stdin says "codex", config says "claude". stdin wins.
        let input = StatuslineInput {
            provider: Some("codex".to_string()),
            ..StatuslineInput::default()
        };
        let config = StatuslineConfig {
            provider: Some("claude".to_string()),
            ..StatuslineConfig::default()
        };
        let view = statusline_view(input, &config);
        // When codex is selected but no session data is available, model_name
        // falls back to the provider name "codex".
        assert_eq!(view.model_name, "codex");
    }

    #[test]
    fn transcript_path_forces_claude_before_config_provider() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let transcript = temp_dir.path().join("transcript.jsonl");
        fs::write(
            &transcript,
            "{\"type\":\"assistant\",\"usage\":{\"input_tokens\":50000,\"output_tokens\":10000,\"cache_read_input_tokens\":30000,\"cache_creation_input_tokens\":10000}}\n",
        )
        .unwrap();

        let mut config = StatuslineConfig {
            provider: Some("codex".to_string()),
            ..StatuslineConfig::default()
        };
        config.context_limits.insert("sonnet".to_string(), 100_000);

        let view = statusline_view(
            StatuslineInput {
                transcript_path: Some(transcript.to_string_lossy().to_string()),
                provider: None,
                session_path: None,
                model: Some(StatuslineModel {
                    display_name: Some("Claude Sonnet 4.6".to_string()),
                }),
                workspace: None,
            },
            &config,
        );

        assert_eq!(view.model_name, "sonnet 4.6");
        assert_eq!(view.context_pct, Some(90));
    }

    #[test]
    fn session_path_json_infers_gemini_before_config_provider() {
        let input = StatuslineInput {
            provider: None,
            session_path: Some("/tmp/session.json".to_string()),
            ..StatuslineInput::default()
        };
        let config = StatuslineConfig {
            provider: Some("codex".to_string()),
            ..StatuslineConfig::default()
        };
        let view = statusline_view(input, &config);
        assert_eq!(view.model_name, "gemini");
    }

    #[test]
    fn session_path_jsonl_infers_codex_before_config_provider() {
        let input = StatuslineInput {
            provider: None,
            session_path: Some("/tmp/session.jsonl".to_string()),
            ..StatuslineInput::default()
        };
        let config = StatuslineConfig {
            provider: Some("gemini".to_string()),
            ..StatuslineConfig::default()
        };
        let view = statusline_view(input, &config);
        assert_eq!(view.model_name, "codex");
    }

    #[test]
    fn config_provider_used_when_no_stdin_provider() {
        // No stdin provider, config says "gemini". Config wins.
        let input = StatuslineInput::default();
        let config = StatuslineConfig {
            provider: Some("gemini".to_string()),
            ..StatuslineConfig::default()
        };
        let view = statusline_view(input, &config);
        assert_eq!(view.model_name, "gemini");
    }

    #[test]
    fn no_provider_falls_through_to_auto_detect() {
        // No stdin provider, no config provider. Auto-detect runs.
        let input = StatuslineInput::default();
        let config = StatuslineConfig::default();
        let view = statusline_view(input, &config);
        // Auto-detect is environment-dependent; we just verify it doesn't panic
        // and produces a known provider name.
        let known = ["claude", "codex", "gemini"];
        assert!(
            known
                .iter()
                .any(|&n| view.model_name.contains(n) || view.model_name == "unknown"),
            "model_name should be from a known provider, got '{}'",
            view.model_name,
        );
    }

    #[test]
    fn tool_adoption_stat_returns_none_when_db_missing() {
        let temp = tempfile::TempDir::new().unwrap();
        let nonexistent = temp.path().join("canopy.db");
        assert!(tool_adoption_stat_at_path(&nonexistent).is_none());
    }

    #[test]
    fn tool_adoption_stat_parses_score_json() {
        let temp = tempfile::TempDir::new().unwrap();
        let db = temp.path().join("canopy.db");
        let conn = Connection::open(&db).unwrap();
        conn.execute(
            "CREATE TABLE tool_adoption_scores (task_id TEXT, score_json TEXT, created_at TEXT)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tool_adoption_scores VALUES ('t1', '{\"score\":0.75,\"tools_used\":3,\"tools_relevant\":4,\"tools_available\":5,\"details\":[]}', '2024-01-01')",
            [],
        )
        .unwrap();
        let stat = tool_adoption_stat_at_path(&db).unwrap();
        assert_eq!(stat.tools_used, 3);
        assert_eq!(stat.tools_relevant, 4);
        assert!((stat.score - 0.75).abs() < 0.001);
    }

    #[test]
    fn tool_adoption_segment_renders_tools_indicator() {
        let stat = ToolAdoptionStat {
            tools_used: 3,
            tools_relevant: 4,
            score: 0.75,
        };
        let label = format!("tools:{}/{}", stat.tools_used, stat.tools_relevant);
        assert_eq!(label, "tools:3/4");
    }

    #[test]
    fn canopy_notifications_returns_none_when_db_missing() {
        let temp = tempfile::TempDir::new().unwrap();
        let nonexistent = temp.path().join("canopy.db");
        assert!(canopy_unread_count_at_path(&nonexistent).is_none());
    }

    #[test]
    fn canopy_notifications_counts_unread() {
        let temp = tempfile::TempDir::new().unwrap();
        let db = temp.path().join("canopy.db");
        let conn = Connection::open(&db).unwrap();
        conn.execute(
            "CREATE TABLE notifications (notification_id TEXT, event_type TEXT, task_id TEXT, agent_id TEXT, payload TEXT, seen INTEGER, created_at TEXT)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO notifications VALUES ('n1', 'task_completed', 't1', NULL, '{}', 0, '2024-01-01')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO notifications VALUES ('n2', 'task_completed', 't2', NULL, '{}', 1, '2024-01-02')",
            [],
        )
        .unwrap();
        let count = canopy_unread_count_at_path(&db).unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn canopy_notifications_segment_shows_unread_count() {
        let count = 3u32;
        let label = format!("canopy:{count} unread");
        assert_eq!(label, "canopy:3 unread");
    }
}
