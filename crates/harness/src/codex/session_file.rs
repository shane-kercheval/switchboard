//! Codex session-file lookup, parsing, and post-turn enrichment.
//!
//! After each turn's terminal stream event (`turn.completed` / `turn.failed`),
//! the Codex adapter reads the session file to fill in metadata the stream
//! omits. Per `docs/research/codex-cli-observed.md`, the session file is
//! the **only** source for:
//! - `event_msg/task_started.payload.model_context_window` →
//!   `TurnEnd.usage.context_window` (per-turn).
//! - `event_msg/token_count.rate_limits` (non-null variant only) →
//!   `RateLimitEvent.info` (per-turn).
//! - `session_meta.payload.cli_version` → `SessionMeta.harness_version` (once
//!   per session).
//! - `turn_context.payload.model` (first one in file) → `SessionMeta.model`
//!   (once per session).
//!
//! ## ID-space distinction
//!
//! Switchboard `TurnId` is dispatcher-local (UUID v7 we generate). Codex
//! session-file `turn_id` is harness-local (UUID v7 Codex generates). The two
//! **never** match by design. Per-turn selection is by **last-record-in-file
//! at terminal-event time**, not by id — Codex writes the session file
//! synchronously, so by the time `turn.completed` arrives, the last
//! `task_started` is the current turn's. A future cleanup that "matches by
//! `turn_id`" would silently match nothing.
//!
//! ## Path resolution
//!
//! Codex session files live at
//! `<home>/.codex/sessions/<YYYY>/<MM>/<DD>/rollout-*-<session-uuid>.jsonl`.
//! **Codex partitions by local date, not UTC** — the directory key is
//! captured from `chrono::Local::now().date_naive()` at first dispatch and
//! stored in the sidecar as `session_partition_date`. Codex appends to the
//! original-partition file even on cross-day resumes; the stored date is
//! authoritative across local-date boundaries. **Never recompute the date
//! from any wall-clock function at enrichment time** — always read from
//! the sidecar. See `docs/research/codex-cli-observed.md` for the
//! verification evidence and the fallback path if Codex ever changes
//! partition behavior.
//!
//! ## `raw` field policy
//!
//! `SessionMeta.raw` carries the `session_meta` line for future forward-compat
//! field promotion. Codex's `session_meta.payload.base_instructions.text` is
//! the entire model system prompt (5–20KB) — never UI-rendered, but included
//! in the unstripped raw it would dominate the IPC payload. Strip the `text`
//! field of `base_instructions` to a sentinel; preserve the rest of the
//! envelope verbatim so the surrounding shape stays observable.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, NaiveDate, Utc};
use serde_json::Value;
use switchboard_core::AgentId;
use uuid::Uuid;

use crate::events::{ContentKind, McpServerStatus, ToolKind, TurnId, TurnUsage};
use crate::transcript::{
    LoadTranscriptError, LoadedTranscript, ParseWarning, SessionMetaInfo, Turn, TurnItem,
    TurnStatus, merge_meta_with_loaders, stale_sidecar_warning,
};

use super::config::load_mcp_servers;
use super::skills::load_skills;

/// Per-attempt backoff between session-file read tries. Codex writes the
/// session file synchronously per `docs/research/codex-cli-observed.md`; by
/// the time the terminal stream event arrives, the file should already be
/// on disk. The first attempt fires **immediately** — the backoff applies
/// only between failed attempts, so a typical turn pays zero latency. Two
/// backoffs across three total attempts cap worst-case enrichment latency at
/// 400ms before giving up. Tune downward only with empirical evidence.
pub const ENRICHMENT_RETRY_DELAY_MS: u64 = 200;

/// What enrichment extracted from the session file. All fields optional —
/// any subset may be missing if the file isn't readable or doesn't carry the
/// expected records. The adapter degrades gracefully per-field.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Enrichment {
    /// From the last `event_msg/task_started` record in the file. Used to
    /// fill `TurnEnd.usage.context_window`.
    pub context_window: Option<u32>,
    /// From the last `event_msg/token_count` record with non-null
    /// `rate_limits`. Used as `RateLimitEvent.info`. Carried as raw JSON
    /// because the rate-limit shape is "opaque to consumers" per
    /// `docs/system-design.md`.
    pub rate_limits: Option<Value>,
    /// From `session_meta.payload.cli_version` (line 1). Used for
    /// `SessionMeta.harness_version`.
    pub cli_version: Option<String>,
    /// From the first `turn_context.payload.model` in the file. Used for
    /// `SessionMeta.model`. Codex supports per-turn model overrides; the
    /// first-turn model is the authoritative session-level snapshot for
    /// `SessionMeta`.
    pub model: Option<String>,
    /// The full `session_meta` line as JSON, with
    /// `payload.base_instructions.text` replaced by a sentinel. Used as
    /// `SessionMeta.raw`. `None` if line 1 isn't a `session_meta` record.
    pub session_meta_raw: Option<Value>,
}

/// Compute the canonical session-file path glob's parent directory for a
/// given start date. Layout: `<home>/.codex/sessions/YYYY/MM/DD/`.
/// `%Y` / `%m` / `%d` already zero-pad to the expected widths.
#[must_use]
pub fn session_directory(home_dir: &Path, session_partition_date: NaiveDate) -> PathBuf {
    home_dir
        .join(".codex")
        .join("sessions")
        .join(session_partition_date.format("%Y").to_string())
        .join(session_partition_date.format("%m").to_string())
        .join(session_partition_date.format("%d").to_string())
}

/// Locate the session file for `session_id` under `home_dir` for the given
/// original-start-date. Codex's filenames are
/// `rollout-<timestamp>-<session-uuid>.jsonl`; the only unknown is the
/// timestamp, so we match by suffix. Returns `None` if the directory or file
/// is absent.
///
/// On multi-match (very rare — would require a backup/rename or Codex bug,
/// since session UUIDs are unique by construction), picks the file with the
/// **latest mtime**, falling back to lexicographic order if mtime is
/// unavailable. The "newest wins" rule avoids silently enriching from a
/// stale duplicate.
///
/// A `glob` crate dep is unnecessary for one suffix-match pattern — a single
/// `read_dir` + suffix filter is simpler, has no allocations beyond the
/// filename strings, and avoids pulling in a transitive dep tree.
#[must_use]
pub fn locate_session_file(
    home_dir: &Path,
    session_partition_date: NaiveDate,
    session_id: &str,
) -> Option<PathBuf> {
    let dir = session_directory(home_dir, session_partition_date);
    let entries = std::fs::read_dir(&dir).ok()?;
    let suffix = format!("-{session_id}.jsonl");
    let mut matches: Vec<PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if let Some(name) = path.file_name().and_then(|n| n.to_str())
            && name.starts_with("rollout-")
            && name.ends_with(&suffix)
        {
            matches.push(path);
        }
    }
    pick_newest(matches)
}

/// Choose the most-recent path by mtime. Falls back to the lexicographically
/// largest filename if mtime can't be read (filesystems without timestamp
/// support, permission edge cases) — Codex's `rollout-<timestamp>-` filename
/// prefix happens to make lex-largest correlate with newest in practice.
fn pick_newest(mut matches: Vec<PathBuf>) -> Option<PathBuf> {
    match matches.len() {
        0 => None,
        1 => matches.pop(),
        _ => {
            matches.sort_by(|a, b| {
                let mtime_a = a.metadata().and_then(|m| m.modified()).ok();
                let mtime_b = b.metadata().and_then(|m| m.modified()).ok();
                match (mtime_a, mtime_b) {
                    (Some(ma), Some(mb)) => ma.cmp(&mb),
                    _ => a.file_name().cmp(&b.file_name()),
                }
            });
            matches.pop() // largest after ascending sort
        }
    }
}

/// Error from `find_codex_session_file_for_attach`. Distinct from
/// `locate_session_file`'s "newest-mtime-wins" silent contract because the
/// attach flow commits a Switchboard agent to one specific session file for
/// its lifetime — picking arbitrarily on a multi-match (or silently failing
/// on a miss) would bind to the wrong harness session and violate the
/// session-id-uniqueness invariant.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AttachLookupError {
    /// No `rollout-*-<session_id>.jsonl` file exists under
    /// `~/.codex/sessions/*/*/*/`.
    #[error("no Codex session file found for session_id {session_id}")]
    NotFound { session_id: String },
    /// More than one `rollout-*-<session_id>.jsonl` file exists across the
    /// date partitions. Impossible by Codex's design (UUIDs are unique by
    /// construction); a real occurrence implies external anomaly (manual copy,
    /// FS corruption). Surface to the user rather than picking arbitrarily.
    #[error("ambiguous Codex session file for session_id {session_id}: {} candidates", paths.len())]
    Ambiguous {
        session_id: String,
        paths: Vec<PathBuf>,
    },
}

/// Locate the Codex session file for an *existing* `session_id`, scanning
/// **all** date partitions under `~/.codex/sessions/`. Returns the file path
/// and the parsed `YYYY-MM-DD` from the directory tree (load-bearing for the
/// attach-flow sidecar's `session_partition_date`).
///
/// **Distinct from `locate_session_file`.** `locate_session_file` is used by
/// post-turn enrichment, where the agent has already committed to a
/// `session_id` + date pair (the sidecar carries both); silently picking
/// newest-mtime on a duplicate is acceptable. This attach helper is used
/// **before** registration commits, and the user is choosing which file to
/// bind to — silent dup resolution would bind to the wrong file. Fail loud.
///
/// Scan strategy: `read_dir × 3` over `<home>/.codex/sessions/YYYY/MM/DD/`.
/// Non-numeric directory names are silently skipped (defensive against
/// `.DS_Store` and similar). The whole scan errors only if the root
/// `~/.codex/sessions/` directory is unreadable; per-leaf read failures are
/// skipped so a single permission-denied date dir doesn't blanket-fail the
/// lookup.
pub fn find_codex_session_file_for_attach(
    home_dir: &Path,
    session_id: &str,
) -> Result<(PathBuf, NaiveDate), AttachLookupError> {
    let root = home_dir.join(".codex").join("sessions");
    let suffix = format!("-{session_id}.jsonl");
    let mut matches: Vec<(PathBuf, NaiveDate)> = Vec::new();

    let Ok(year_entries) = std::fs::read_dir(&root) else {
        return Err(AttachLookupError::NotFound {
            session_id: session_id.to_owned(),
        });
    };
    for year_entry in year_entries.flatten() {
        let Some(year) = parse_numeric_dir(&year_entry, 4) else {
            continue;
        };
        let Ok(month_entries) = std::fs::read_dir(year_entry.path()) else {
            continue;
        };
        for month_entry in month_entries.flatten() {
            let Some(month) = parse_numeric_dir(&month_entry, 2) else {
                continue;
            };
            let Ok(day_entries) = std::fs::read_dir(month_entry.path()) else {
                continue;
            };
            for day_entry in day_entries.flatten() {
                let Some(day) = parse_numeric_dir(&day_entry, 2) else {
                    continue;
                };
                let Some(date) =
                    NaiveDate::from_ymd_opt(i32::from(year), u32::from(month), u32::from(day))
                else {
                    continue;
                };
                let Ok(file_entries) = std::fs::read_dir(day_entry.path()) else {
                    continue;
                };
                for file_entry in file_entries.flatten() {
                    let path = file_entry.path();
                    if let Some(name) = path.file_name().and_then(|n| n.to_str())
                        && name.starts_with("rollout-")
                        && name.ends_with(&suffix)
                    {
                        matches.push((path, date));
                    }
                }
            }
        }
    }

    match matches.len() {
        0 => Err(AttachLookupError::NotFound {
            session_id: session_id.to_owned(),
        }),
        1 => Ok(matches.into_iter().next().expect("len==1 guaranteed")),
        _ => {
            // Sort for stable error output.
            matches.sort_by(|a, b| a.0.cmp(&b.0));
            Err(AttachLookupError::Ambiguous {
                session_id: session_id.to_owned(),
                paths: matches.into_iter().map(|(p, _)| p).collect(),
            })
        }
    }
}

/// Parse a directory-entry name as a fixed-width zero-padded numeric (year=4,
/// month/day=2). Returns None for non-numeric names (`.DS_Store`, `Thumbs.db`,
/// stray files, etc.) and for unexpected widths. `u16` accommodates 4-digit
/// years through 9999 — well past any realistic session date.
fn parse_numeric_dir(entry: &std::fs::DirEntry, expected_width: usize) -> Option<u16> {
    let name = entry.file_name();
    let name_str = name.to_str()?;
    if name_str.len() != expected_width || !name_str.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    name_str.parse().ok()
}

/// Read and parse the session file. Returns `Enrichment::default()` (all
/// `None`) on any IO error or top-level corruption — per the harness-owned
/// file skip-with-warning invariant in `AGENTS.md`. Individual malformed
/// lines are warned-and-skipped, valid lines preserved.
#[must_use]
pub fn parse_session_file(path: &Path) -> Enrichment {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "Codex session-file read failed; enrichment degraded"
            );
            return Enrichment::default();
        }
    };
    parse_session_content(&content)
}

/// Parse already-loaded session-file content. Exposed for testing without
/// the FS read.
#[must_use]
pub fn parse_session_content(content: &str) -> Enrichment {
    let mut enrichment = Enrichment::default();
    let mut model_set = false; // first-turn_context wins (set-once gate)

    for (idx, line) in content.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let value: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    line = idx + 1,
                    error = %e,
                    "Codex session-file: malformed JSON line; skipping"
                );
                continue;
            }
        };

        let record_type = value.get("type").and_then(Value::as_str).unwrap_or("");
        let payload = value.get("payload");

        match record_type {
            "session_meta" => {
                if let Some(p) = payload
                    && let Some(version) = p.get("cli_version").and_then(Value::as_str)
                {
                    enrichment.cli_version = Some(version.to_owned());
                }
                enrichment.session_meta_raw = Some(strip_base_instructions(value));
            }
            "turn_context" => {
                if !model_set
                    && let Some(p) = payload
                    && let Some(model) = p.get("model").and_then(Value::as_str)
                {
                    enrichment.model = Some(model.to_owned());
                    model_set = true;
                }
            }
            "event_msg" => {
                let Some(p) = payload else { continue };
                let event_type = p.get("type").and_then(Value::as_str).unwrap_or("");
                match event_type {
                    "task_started" => {
                        // Last-task_started-wins. On resumed sessions the
                        // file accumulates one task_started per turn; the
                        // current turn's is the most recent.
                        if let Some(window) = p.get("model_context_window").and_then(Value::as_u64)
                        {
                            enrichment.context_window = u32::try_from(window).ok();
                        }
                    }
                    "token_count" => {
                        // Two variants share this type — only the one with
                        // non-null `rate_limits` is what enrichment surfaces.
                        // The info-only variant is ignored (the stream's
                        // turn.completed.usage carries token totals).
                        // Last-rate-limit-bearing-record-wins.
                        if let Some(rate_limits) = p.get("rate_limits")
                            && !rate_limits.is_null()
                        {
                            enrichment.rate_limits = Some(rate_limits.clone());
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    enrichment
}

/// Strip `payload.base_instructions.text` from a `session_meta` record. The
/// surrounding envelope is preserved verbatim so future consumers can still
/// introspect the field's existence and the `base_instructions` table's
/// other keys. Returns a clone — the caller owns the result.
fn strip_base_instructions(mut value: Value) -> Value {
    if let Some(payload) = value.get_mut("payload")
        && let Some(base) = payload.get_mut("base_instructions")
        && let Some(text) = base.get_mut("text")
        && text.is_string()
    {
        *text = Value::String("<stripped — see codex-cli-observed.md>".to_owned());
    }
    value
}

/// Hook trait for the retry loop's sleep. Production uses `TokioSleeper`;
/// tests inject a no-op or a counter to assert retry behavior without
/// wall-clock waits. Trivial trait — kept inline so the surface is local.
#[async_trait::async_trait]
pub trait Sleeper: Send + Sync {
    async fn sleep(&self, duration: Duration);
}

/// Production [`Sleeper`] — wraps `tokio::time::sleep`.
pub struct TokioSleeper;

#[async_trait::async_trait]
impl Sleeper for TokioSleeper {
    async fn sleep(&self, duration: Duration) {
        tokio::time::sleep(duration).await;
    }
}

/// Locate and parse the session file with bounded retries. Attempts the
/// read immediately; on miss, sleeps `ENRICHMENT_RETRY_DELAY_MS` and retries,
/// up to a total of three attempts (two backoffs). On all-miss returns
/// `Enrichment::default()` and logs a warning — the adapter then emits
/// `TurnEnd` with `context_window: None` and no enrichment-derived events.
///
/// **Typical-case latency: 0ms.** Codex writes the session file
/// synchronously and the file is usually on disk by terminal-event time;
/// only flush-latency edge cases trigger the retries.
pub async fn load_with_retry(
    home_dir: &Path,
    session_partition_date: NaiveDate,
    session_id: &str,
    sleeper: &dyn Sleeper,
) -> Enrichment {
    const ATTEMPTS: usize = 3;
    for attempt in 0..ATTEMPTS {
        if attempt > 0 {
            sleeper
                .sleep(Duration::from_millis(ENRICHMENT_RETRY_DELAY_MS))
                .await;
        }
        if let Some(path) = locate_session_file(home_dir, session_partition_date, session_id) {
            return parse_session_file(&path);
        }
    }
    tracing::warn!(
        session_id = %session_id,
        date = %session_partition_date,
        "Codex session file not found after retry; TurnEnd will lack enriched fields"
    );
    Enrichment::default()
}

/// Project the enrichment + scoped registries onto a `SessionMeta` event
/// payload. Returns `None` if neither model nor `cli_version` was extracted
/// (the file was unreadable or contained no recognizable records) — emitting
/// a `SessionMeta` with both required fields empty would carry no information.
#[must_use]
pub fn build_session_meta_fields(
    enrichment: &Enrichment,
    mcp_servers: Vec<McpServerStatus>,
    skills: Vec<String>,
) -> Option<SessionMetaFields> {
    if enrichment.model.is_none() && enrichment.cli_version.is_none() {
        return None;
    }
    Some(SessionMetaFields {
        model: enrichment.model.clone().unwrap_or_default(),
        harness_version: enrichment.cli_version.clone().unwrap_or_default(),
        mcp_servers,
        skills,
        raw: enrichment.session_meta_raw.clone().unwrap_or(Value::Null),
    })
}

/// Fields ready to plug into [`crate::events::AdapterEvent::SessionMeta`].
/// `tools` is always `vec![]` for Codex — no equivalent registry source on
/// disk; kept implicit on the adapter side rather than carried here.
pub struct SessionMetaFields {
    pub model: String,
    pub harness_version: String,
    pub mcp_servers: Vec<McpServerStatus>,
    pub skills: Vec<String>,
    pub raw: Value,
}

/// Load a Codex session file and project it into a
/// [`crate::transcript::LoadedTranscript`]. Used by transcript hydration on
/// project open and on attach.
///
/// `session_partition_date` MUST come from the per-agent session-link sidecar
/// (`crate::codex::sidecar::SessionLinkRecord.session_partition_date`). Codex
/// partitions session files by **local date** at first dispatch and resumes
/// append to the original-partition file across local-date boundaries; the
/// stored date is authoritative — never recompute from `Local::today()`.
///
/// `cwd` is the user's bound working directory, used for project-scoped
/// MCP config and skill loaders (the same loaders live dispatch uses).
///
/// **Stale-sidecar case**: if `session_partition_date` is present but no
/// session file lives at the recorded path (user deleted it, external
/// rotation), returns `Ok(LoadedTranscript { turns: vec![], warnings: vec![<stale-sidecar warning>] })`.
/// **Missing-sidecar case** (agent created, never dispatched): caller passes
/// `None` for the date — returns `Ok(LoadedTranscript::default())` with no
/// warning.
pub fn load_codex_transcript(
    home_dir: &Path,
    cwd: &Path,
    session_id: &str,
    session_partition_date: Option<NaiveDate>,
    agent_id: AgentId,
) -> Result<LoadedTranscript, LoadTranscriptError> {
    let Some(date) = session_partition_date else {
        // Agent has no sidecar yet — created but never dispatched.
        // Surface meta (loaded from config files) even with empty turns
        // so the sidebar's model / registries populate the moment the
        // agent is selected.
        return Ok(LoadedTranscript {
            meta: Some(merge_meta_with_loaders(
                None,
                load_mcp_servers(home_dir, cwd),
                load_skills(home_dir, cwd),
            )),
            ..LoadedTranscript::default()
        });
    };

    let Some(path) = locate_session_file(home_dir, date, session_id) else {
        return Ok(LoadedTranscript {
            meta: Some(merge_meta_with_loaders(
                None,
                load_mcp_servers(home_dir, cwd),
                load_skills(home_dir, cwd),
            )),
            warnings: vec![stale_sidecar_warning()],
            ..LoadedTranscript::default()
        });
    };

    let content = std::fs::read_to_string(&path)?;

    let mut transcript = parse_codex_transcript_content(&content, agent_id);
    transcript.meta = Some(merge_meta_with_loaders(
        transcript.meta.take(),
        load_mcp_servers(home_dir, cwd),
        load_skills(home_dir, cwd),
    ));
    Ok(transcript)
}

/// Parse Codex session-file content into a `LoadedTranscript` (no FS access).
/// Exposed `pub(crate)` for unit tests that want to drive the parser without
/// staging a temp file.
pub(crate) fn parse_codex_transcript_content(content: &str, agent_id: AgentId) -> LoadedTranscript {
    let mut state = CodexReconstruction::new(agent_id);
    for (idx, line) in content.lines().enumerate() {
        let line_number = idx + 1;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(line) {
            Ok(record) => state.ingest(line_number, &record),
            Err(e) => state.warn(line_number, format!("malformed JSON: {e}")),
        }
    }
    let mut t = state.finalize();
    // Use the existing enrichment parser to extract model/cli_version/last
    // rate_limits, then merge into our LoadedTranscript shape. Single source
    // of truth for meta fields.
    let enrichment = parse_session_content(content);
    t.last_rate_limit = enrichment.rate_limits;
    t.meta = Some(SessionMetaInfo {
        model: enrichment.model.unwrap_or_default(),
        harness_version: enrichment.cli_version.unwrap_or_default(),
        tools: vec![],
        mcp_servers: vec![],
        skills: vec![],
    });
    t
}

/// In-progress reconstruction state. Walks records in order, opening agent
/// turns on `task_started` and closing on `task_complete` or EOF.
struct CodexReconstruction {
    agent_id: AgentId,
    turns: Vec<Turn>,
    current_agent: Option<CodexAgentBuilder>,
    warnings: Vec<ParseWarning>,
}

struct CodexAgentBuilder {
    turn_id: TurnId,
    agent_id: AgentId,
    started_at: DateTime<Utc>,
    last_seen_at: DateTime<Utc>,
    items: Vec<TurnItem>,
    usage: Option<TurnUsage>,
    context_window: Option<u32>,
    pending_mcp_results: HashMap<String, McpResult>,
}

/// Captured `mcp_tool_call_end` payload — applied to the matching
/// `function_call` item when both have been observed.
struct McpResult {
    server: String,
    tool: String,
    output: String,
    is_error: bool,
    completed_at: Option<DateTime<Utc>>,
}

impl CodexReconstruction {
    fn new(agent_id: AgentId) -> Self {
        Self {
            agent_id,
            turns: Vec::new(),
            current_agent: None,
            warnings: Vec::new(),
        }
    }

    fn warn(&mut self, line_number: usize, reason: impl Into<String>) {
        self.warnings.push(ParseWarning {
            line_number,
            reason: reason.into(),
        });
    }

    fn ingest(&mut self, line_number: usize, record: &Value) {
        let record_type = record.get("type").and_then(Value::as_str).unwrap_or("");
        let payload = record.get("payload");
        let timestamp = record
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc));

        match record_type {
            "event_msg" => self.handle_event_msg(line_number, payload, timestamp),
            "response_item" => self.handle_response_item(line_number, payload, timestamp),
            _ => {}
        }
    }

    #[allow(clippy::too_many_lines)]
    fn handle_event_msg(
        &mut self,
        line_number: usize,
        payload: Option<&Value>,
        timestamp: Option<DateTime<Utc>>,
    ) {
        let Some(p) = payload else { return };
        let event_type = p.get("type").and_then(Value::as_str).unwrap_or("");
        match event_type {
            "task_started" => {
                // Open a fresh agent turn. Close any predecessor first
                // (defensive against missing task_complete records).
                self.close_current_agent(TurnStatus::Failed);
                let started_at = timestamp.unwrap_or_else(Utc::now);
                let context_window = p
                    .get("model_context_window")
                    .and_then(Value::as_u64)
                    .and_then(|v| u32::try_from(v).ok());
                self.current_agent = Some(CodexAgentBuilder {
                    turn_id: Uuid::now_v7(),
                    agent_id: self.agent_id,
                    started_at,
                    last_seen_at: started_at,
                    items: Vec::new(),
                    usage: None,
                    context_window,
                    pending_mcp_results: HashMap::new(),
                });
            }
            "task_complete" => {
                self.close_current_agent(TurnStatus::Complete);
            }
            "user_message" => {
                // Push to `self.turns` directly, not into `builder.items`:
                // Codex emits `task_started` BEFORE `user_message`, so the
                // agent builder is already open here. The user turn should
                // appear chronologically before the agent turn the builder
                // will eventually close (on `task_complete`). Since the
                // open agent turn isn't yet in `self.turns`, a direct push
                // at the current tail naturally places the user turn first;
                // the agent turn slots in after on close.
                let Some(message) = p.get("message").and_then(Value::as_str) else {
                    return;
                };
                let started_at = timestamp.unwrap_or_else(Utc::now);
                let user_turn = Turn::User {
                    turn_id: Uuid::now_v7(),
                    agent_id: self.agent_id,
                    started_at,
                    text: message.to_owned(),
                };
                self.turns.push(user_turn);
            }
            "agent_message" => {
                let Some(message) = p.get("message").and_then(Value::as_str) else {
                    return;
                };
                if let Some(builder) = self.current_agent.as_mut() {
                    builder.items.push(TurnItem::Text {
                        kind: ContentKind::Text,
                        text: message.to_owned(),
                    });
                    if let Some(t) = timestamp {
                        builder.last_seen_at = t;
                    }
                }
            }
            "token_count" => {
                // `info.last_token_usage` carries per-turn tokens. `info` is
                // null on the rate-limits-only variant — skip those.
                let Some(builder) = self.current_agent.as_mut() else {
                    return;
                };
                let Some(info) = p.get("info").filter(|v| !v.is_null()) else {
                    return;
                };
                let last = info.get("last_token_usage").unwrap_or(info);
                let input = last
                    .get("input_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                let output = last
                    .get("output_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                builder.usage = Some(TurnUsage {
                    input_tokens: input,
                    output_tokens: output,
                    cached_input_tokens: last.get("cached_input_tokens").and_then(Value::as_u64),
                    reasoning_output_tokens: last
                        .get("reasoning_output_tokens")
                        .and_then(Value::as_u64),
                    context_window: builder.context_window,
                    total_cost_usd: None,
                });
            }
            "mcp_tool_call_end" => {
                let Some(call_id) = p.get("call_id").and_then(Value::as_str) else {
                    self.warn(line_number, "mcp_tool_call_end missing call_id");
                    return;
                };
                let invocation = p.get("invocation");
                let server = invocation
                    .and_then(|i| i.get("server"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let tool = invocation
                    .and_then(|i| i.get("tool"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let (output, is_error) = decode_mcp_result(p.get("result"));
                let result = McpResult {
                    server,
                    tool,
                    output,
                    is_error,
                    completed_at: timestamp,
                };
                if let Some(builder) = self.current_agent.as_mut() {
                    let matched = apply_mcp_result(&mut builder.items, call_id, &result);
                    // Only stash for late-arrival pairing if the eager apply
                    // didn't match (rare — Codex emits function_call first).
                    // Stashing on match would leak unused entries.
                    if !matched {
                        builder
                            .pending_mcp_results
                            .insert(call_id.to_owned(), result);
                    }
                }
            }
            _ => {}
        }
    }

    fn handle_response_item(
        &mut self,
        line_number: usize,
        payload: Option<&Value>,
        timestamp: Option<DateTime<Utc>>,
    ) {
        let Some(p) = payload else { return };
        let item_type = p.get("type").and_then(Value::as_str).unwrap_or("");
        match item_type {
            "function_call" => {
                let Some(call_id) = p.get("call_id").and_then(Value::as_str) else {
                    self.warn(line_number, "function_call missing call_id");
                    return;
                };
                let raw_name = p.get("name").and_then(Value::as_str).unwrap_or("");
                let arguments = p
                    .get("arguments")
                    .and_then(Value::as_str)
                    .and_then(|s| serde_json::from_str::<Value>(s).ok())
                    .unwrap_or(Value::Null);
                let namespace = p.get("namespace").and_then(Value::as_str);
                let (kind, name) = classify_codex_function_call(raw_name, namespace);
                let started_at = timestamp.unwrap_or_else(Utc::now);
                let Some(builder) = self.current_agent.as_mut() else {
                    return;
                };
                let item = TurnItem::Tool {
                    tool_use_id: call_id.to_owned(),
                    kind,
                    name,
                    input: arguments,
                    output: None,
                    is_error: None,
                    started_at,
                    completed_at: None,
                };
                builder.items.push(item);
                // If the matching mcp_tool_call_end already arrived, apply
                // it now (shouldn't happen in practice — Codex writes the
                // function_call before the end event — but defensive).
                if let Some(result) = builder.pending_mcp_results.remove(call_id) {
                    let _ = apply_mcp_result(&mut builder.items, call_id, &result);
                }
            }
            "function_call_output" => {
                let Some(call_id) = p.get("call_id").and_then(Value::as_str) else {
                    self.warn(line_number, "function_call_output missing call_id");
                    return;
                };
                let output = p
                    .get("output")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let completed_at = timestamp;
                let Some(builder) = self.current_agent.as_mut() else {
                    return;
                };
                let mut matched = false;
                for item in &mut builder.items {
                    if let TurnItem::Tool {
                        tool_use_id,
                        output: out,
                        is_error,
                        completed_at: cat,
                        ..
                    } = item
                        && tool_use_id == call_id
                    {
                        // Don't overwrite an MCP-result-supplied output.
                        if out.is_none() {
                            *out = Some(output.clone());
                            *is_error = Some(false);
                            *cat = completed_at;
                        }
                        matched = true;
                        break;
                    }
                }
                if !matched {
                    self.warn(
                        line_number,
                        format!("function_call_output for {call_id} did not match any open tool"),
                    );
                }
            }
            // `response_item/message` carries the structured model-API form
            // of the conversation content (`content: [{type:"input_text",
            // text:"..."}]`). We don't parse it — `event_msg/user_message`
            // and `event_msg/agent_message` are the UI-friendly summaries
            // that flow alongside in every observed Codex session, and
            // consuming both would double-count text in the rehydrated
            // transcript. Regression check: the session-file unit tests
            // below (`load_codex_transcript_text_only_turn_produces_user_and_agent`
            // and friends) construct fixtures using these `event_msg`
            // records and assert non-empty `items`. If a future Codex
            // release stops emitting `event_msg/agent_message`, those
            // assertions fail before the parser change ships.
            _ => {}
        }
    }

    fn close_current_agent(&mut self, status: TurnStatus) {
        let Some(builder) = self.current_agent.take() else {
            return;
        };
        self.turns.push(Turn::Agent {
            turn_id: builder.turn_id,
            agent_id: builder.agent_id,
            started_at: builder.started_at,
            ended_at: Some(builder.last_seen_at),
            status,
            items: builder.items,
            usage: builder.usage,
        });
    }

    fn finalize(mut self) -> LoadedTranscript {
        // Any in-progress agent turn at EOF is truncated — no task_complete
        // observed before EOF. **Asymmetric with Claude on purpose**: Codex
        // emits an explicit `event_msg/task_complete` per turn, so a missing
        // one means genuine truncation. Claude's session file has no
        // analogous terminal marker; its `finalize` defaults to Complete
        // instead. See `crates/harness/src/claude_code/session_file.rs::
        // ReconstructionState::finalize` for the other side of the
        // asymmetry.
        self.close_current_agent(TurnStatus::Failed);
        LoadedTranscript {
            turns: self.turns,
            meta: None,
            last_rate_limit: None,
            warnings: self.warnings,
        }
    }
}

/// Decode a Codex `mcp_tool_call_end.result`. Variants:
/// - `{"Ok": {"content": [{"type":"text","text":"..."}], "isError": false}}`
/// - `{"Err": "error message"}`
///
/// Returns `(output_string, is_error)`.
fn decode_mcp_result(result: Option<&Value>) -> (String, bool) {
    let Some(result) = result else {
        return (String::new(), false);
    };
    if let Some(ok) = result.get("Ok") {
        let is_error = ok.get("isError").and_then(Value::as_bool).unwrap_or(false);
        let content = ok.get("content").and_then(Value::as_array);
        let text = content
            .map(|blocks| {
                blocks
                    .iter()
                    .filter_map(|b| {
                        if b.get("type").and_then(Value::as_str) == Some("text") {
                            b.get("text").and_then(Value::as_str).map(str::to_owned)
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        (text, is_error)
    } else if let Some(err) = result.get("Err") {
        let msg = err.as_str().unwrap_or("").to_owned();
        (msg, true)
    } else {
        (String::new(), false)
    }
}

/// Discriminate built-in vs MCP function-call name. MCP calls carry a
/// `namespace: "mcp__<server>__"` field; the surfaced name is
/// `<server>.<tool>` (matching the stream-side emission).
fn classify_codex_function_call(name: &str, namespace: Option<&str>) -> (ToolKind, String) {
    if let Some(ns) = namespace
        && ns.starts_with("mcp__")
    {
        let server = ns.trim_start_matches("mcp__").trim_end_matches("__");
        return (ToolKind::Mcp, format!("{server}.{name}"));
    }
    (ToolKind::Builtin, name.to_owned())
}

/// Apply an MCP completion to the matching open tool item. Returns `true`
/// when a matching tool was found and patched.
fn apply_mcp_result(items: &mut [TurnItem], call_id: &str, result: &McpResult) -> bool {
    for item in items {
        if let TurnItem::Tool {
            tool_use_id,
            kind,
            name,
            output,
            is_error,
            completed_at,
            ..
        } = item
            && tool_use_id == call_id
        {
            *kind = ToolKind::Mcp;
            if !result.server.is_empty() && !result.tool.is_empty() {
                *name = format!("{}.{}", result.server, result.tool);
            }
            *output = Some(result.output.clone());
            *is_error = Some(result.is_error);
            *completed_at = result.completed_at;
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use std::sync::Mutex;
    use tempfile::TempDir;

    fn fixture_path(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/codex")
            .join(name)
    }

    #[test]
    fn parse_rate_limits_fixture_extracts_all_four_fields() {
        let content = std::fs::read_to_string(fixture_path("rate-limits.session.jsonl")).unwrap();
        let enrichment = parse_session_content(&content);

        assert_eq!(
            enrichment.context_window,
            Some(258_400),
            "task_started.model_context_window must be extracted"
        );
        assert_eq!(
            enrichment.cli_version.as_deref(),
            Some("0.130.0"),
            "session_meta.cli_version must be extracted"
        );
        assert!(enrichment.rate_limits.is_some(), "rate_limits extracted");
        assert!(
            enrichment.session_meta_raw.is_some(),
            "session_meta line preserved as raw"
        );
        // The fixture has no turn_context records — model stays None.
        assert!(
            enrichment.model.is_none(),
            "no turn_context in fixture → model is None"
        );
    }

    #[test]
    fn parse_extracts_model_from_first_turn_context() {
        let content = r#"
{"type":"session_meta","payload":{"cli_version":"0.130.0"}}
{"type":"turn_context","payload":{"model":"gpt-5.5","cwd":"/x"}}
{"type":"turn_context","payload":{"model":"gpt-5.6","cwd":"/x"}}
"#;
        let enrichment = parse_session_content(content);
        assert_eq!(
            enrichment.model.as_deref(),
            Some("gpt-5.5"),
            "first turn_context.model wins"
        );
    }

    #[test]
    fn parse_filters_token_count_info_only_variant() {
        // The info-only token_count (rate_limits: null) must be ignored; the
        // stream's turn.completed.usage carries token telemetry. Only the
        // rate-limits-bearing variant feeds RateLimitEvent.
        let content = r#"
{"type":"session_meta","payload":{"cli_version":"0.130.0"}}
{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{}},"rate_limits":null}}
"#;
        let enrichment = parse_session_content(content);
        assert!(
            enrichment.rate_limits.is_none(),
            "info-only token_count must not populate rate_limits"
        );
    }

    #[test]
    fn parse_takes_last_task_started_for_context_window() {
        // Resumed-session file: two task_started records, second turn's
        // model_context_window is what we want for the current turn.
        let content = r#"
{"type":"session_meta","payload":{"cli_version":"0.130.0"}}
{"type":"event_msg","payload":{"type":"task_started","model_context_window":200000}}
{"type":"event_msg","payload":{"type":"task_started","model_context_window":300000}}
"#;
        let enrichment = parse_session_content(content);
        assert_eq!(
            enrichment.context_window,
            Some(300_000),
            "last task_started wins"
        );
    }

    #[test]
    fn parse_takes_last_rate_limit_bearing_token_count() {
        let content = r#"
{"type":"event_msg","payload":{"type":"token_count","rate_limits":{"primary":{"used_percent":10.0}}}}
{"type":"event_msg","payload":{"type":"token_count","rate_limits":{"primary":{"used_percent":50.0}}}}
"#;
        let enrichment = parse_session_content(content);
        let rate_limits = enrichment.rate_limits.expect("rate_limits captured");
        // The second record's percent must win.
        assert_eq!(
            rate_limits.pointer("/primary/used_percent"),
            Some(&Value::from(50.0))
        );
    }

    #[test]
    fn parse_strips_base_instructions_text_from_raw() {
        let content = r#"
{"type":"session_meta","payload":{"cli_version":"0.130.0","base_instructions":{"text":"this is a very long system prompt that would bloat IPC"}}}
"#;
        let enrichment = parse_session_content(content);
        let raw = enrichment.session_meta_raw.expect("raw captured");
        assert_eq!(
            raw.pointer("/payload/base_instructions/text"),
            Some(&Value::String(
                "<stripped — see codex-cli-observed.md>".to_owned()
            )),
            "base_instructions.text must be stripped"
        );
        // The surrounding shape is preserved so future consumers can introspect.
        assert_eq!(
            raw.pointer("/payload/cli_version"),
            Some(&Value::String("0.130.0".to_owned())),
            "non-stripped fields preserved"
        );
    }

    #[test]
    fn parse_handles_missing_base_instructions_gracefully() {
        // No base_instructions table at all — must not panic.
        let content = r#"{"type":"session_meta","payload":{"cli_version":"0.130.0"}}"#;
        let enrichment = parse_session_content(content);
        let raw = enrichment.session_meta_raw.expect("raw captured");
        assert!(raw.pointer("/payload/cli_version").is_some());
    }

    #[test]
    fn parse_skips_malformed_lines_keeps_valid() {
        let content = r#"
{"type":"session_meta","payload":{"cli_version":"0.130.0"}}
not valid json
{"type":"event_msg","payload":{"type":"task_started","model_context_window":100}}
"#;
        let enrichment = parse_session_content(content);
        assert_eq!(enrichment.cli_version.as_deref(), Some("0.130.0"));
        assert_eq!(enrichment.context_window, Some(100));
    }

    #[test]
    fn locate_session_file_finds_matching_suffix() {
        let tmp = TempDir::new().unwrap();
        let date = NaiveDate::from_ymd_opt(2026, 5, 15).unwrap();
        let session_id = "019e2c5f-aaaa-7000-8000-000000000001";
        let dir = session_directory(tmp.path(), date);
        std::fs::create_dir_all(&dir).unwrap();
        // The actual file with the matching suffix.
        let target = dir.join(format!("rollout-1747000000000-{session_id}.jsonl"));
        std::fs::write(&target, "{}\n").unwrap();
        // A decoy file with a different suffix.
        std::fs::write(
            dir.join("rollout-1747000000000-other-session.jsonl"),
            "{}\n",
        )
        .unwrap();

        let found = locate_session_file(tmp.path(), date, session_id);
        assert_eq!(found.as_deref(), Some(target.as_path()));
    }

    #[test]
    fn locate_session_file_returns_none_when_directory_missing() {
        let tmp = TempDir::new().unwrap();
        let date = NaiveDate::from_ymd_opt(2026, 5, 15).unwrap();
        // No directory exists at all.
        assert!(locate_session_file(tmp.path(), date, "any-id").is_none());
    }

    #[test]
    fn locate_session_file_picks_newest_mtime_on_multi_match() {
        // Real Codex would never produce two rollouts with the same session
        // UUID, but a backup/rename script could. The plan says "if
        // multiple matches, pick most recent" — pin that against the
        // `read_dir`-order ambiguity.
        let tmp = TempDir::new().unwrap();
        let date = NaiveDate::from_ymd_opt(2026, 5, 15).unwrap();
        let session_id = "019e2c5f-aaaa-7000-8000-0000000000aa";
        let dir = session_directory(tmp.path(), date);
        std::fs::create_dir_all(&dir).unwrap();
        let older = dir.join(format!("rollout-1000-{session_id}.jsonl"));
        let newer = dir.join(format!("rollout-9999-{session_id}.jsonl"));
        std::fs::write(&older, "older").unwrap();
        // Sleep just enough to give the newer file a distinct mtime on
        // filesystems with second-resolution timestamps. macOS HFS+ is
        // second-resolution; APFS / ext4 are nanosecond. 1100ms is the
        // tightest cross-platform guarantee.
        std::thread::sleep(Duration::from_millis(1100));
        std::fs::write(&newer, "newer").unwrap();

        let found = locate_session_file(tmp.path(), date, session_id);
        assert_eq!(
            found.as_deref(),
            Some(newer.as_path()),
            "newest mtime wins on multi-match"
        );
    }

    #[test]
    fn locate_session_file_ignores_non_rollout_files() {
        let tmp = TempDir::new().unwrap();
        let date = NaiveDate::from_ymd_opt(2026, 5, 15).unwrap();
        let session_id = "019e2c5f-aaaa-7000-8000-000000000001";
        let dir = session_directory(tmp.path(), date);
        std::fs::create_dir_all(&dir).unwrap();
        // File ends in the right suffix but isn't a rollout file.
        std::fs::write(dir.join(format!("other-{session_id}.jsonl")), "{}\n").unwrap();

        assert!(locate_session_file(tmp.path(), date, session_id).is_none());
    }

    #[test]
    fn locate_session_file_finds_cross_day_when_pointed_at_yesterday() {
        // Cross-midnight test: sidecar's session_partition_date says May
        // 15; host clock would say May 16. Lookup must use the sidecar's
        // stored date (never recompute from any clock) and find the file
        // in May 15's directory.
        let tmp = TempDir::new().unwrap();
        let yesterday = NaiveDate::from_ymd_opt(2026, 5, 15).unwrap();
        let session_id = "019e2c5f-aaaa-7000-8000-000000000001";
        let yesterday_dir = session_directory(tmp.path(), yesterday);
        std::fs::create_dir_all(&yesterday_dir).unwrap();
        let target = yesterday_dir.join(format!("rollout-x-{session_id}.jsonl"));
        std::fs::write(&target, "{}\n").unwrap();

        // Pointed at yesterday → found.
        assert_eq!(
            locate_session_file(tmp.path(), yesterday, session_id).as_deref(),
            Some(target.as_path())
        );
        // Pointed at today → not found (file is in yesterday's dir).
        let today = NaiveDate::from_ymd_opt(2026, 5, 16).unwrap();
        assert!(locate_session_file(tmp.path(), today, session_id).is_none());
    }

    fn write_rollout(tmp: &Path, date: NaiveDate, session_id: &str) -> PathBuf {
        let dir = session_directory(tmp, date);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("rollout-1747000000000-{session_id}.jsonl"));
        std::fs::write(&path, "{}\n").unwrap();
        path
    }

    #[test]
    fn find_for_attach_returns_path_and_parsed_date_on_single_match() {
        let tmp = TempDir::new().unwrap();
        let date = NaiveDate::from_ymd_opt(2026, 5, 15).unwrap();
        let session_id = "019e2c5f-aaaa-7000-8000-000000000001";
        let target = write_rollout(tmp.path(), date, session_id);

        let (path, parsed_date) =
            find_codex_session_file_for_attach(tmp.path(), session_id).unwrap();
        assert_eq!(path, target);
        assert_eq!(parsed_date, date);
    }

    #[test]
    fn find_for_attach_scans_all_date_partitions() {
        // The caller doesn't know the original spawn date; we walk the
        // YYYY/MM/DD tree to find any match.
        let tmp = TempDir::new().unwrap();
        let session_id = "019e2c5f-bbbb-7000-8000-000000000002";
        let date_old = NaiveDate::from_ymd_opt(2025, 12, 1).unwrap();
        let _decoy = write_rollout(tmp.path(), date_old, "different-session-id");
        let date_target = NaiveDate::from_ymd_opt(2026, 4, 20).unwrap();
        let target = write_rollout(tmp.path(), date_target, session_id);

        let (path, parsed_date) =
            find_codex_session_file_for_attach(tmp.path(), session_id).unwrap();
        assert_eq!(path, target);
        assert_eq!(parsed_date, date_target);
    }

    #[test]
    fn find_for_attach_returns_not_found_when_no_match() {
        let tmp = TempDir::new().unwrap();
        let date = NaiveDate::from_ymd_opt(2026, 5, 15).unwrap();
        let _other = write_rollout(tmp.path(), date, "different-session-id");

        let err = find_codex_session_file_for_attach(tmp.path(), "nope-session-id").unwrap_err();
        assert!(
            matches!(err, AttachLookupError::NotFound { ref session_id } if session_id == "nope-session-id")
        );
    }

    #[test]
    fn find_for_attach_returns_not_found_when_sessions_root_missing() {
        // Empty tmp dir, no ~/.codex/sessions/ at all.
        let tmp = TempDir::new().unwrap();
        let err = find_codex_session_file_for_attach(tmp.path(), "any-id").unwrap_err();
        assert!(matches!(err, AttachLookupError::NotFound { .. }));
    }

    #[test]
    fn find_for_attach_fails_loud_on_ambiguous_match() {
        // Same session_id under two date partitions — impossible by Codex's
        // design (UUIDs are unique), but if it happens (manual copy, FS
        // weirdness), attach must surface it rather than binding arbitrarily.
        let tmp = TempDir::new().unwrap();
        let session_id = "019e2c5f-cccc-7000-8000-000000000003";
        let date_a = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        let date_b = NaiveDate::from_ymd_opt(2026, 2, 2).unwrap();
        let path_a = write_rollout(tmp.path(), date_a, session_id);
        let path_b = write_rollout(tmp.path(), date_b, session_id);

        let err = find_codex_session_file_for_attach(tmp.path(), session_id).unwrap_err();
        match err {
            AttachLookupError::Ambiguous {
                session_id: id,
                paths,
            } => {
                assert_eq!(id, session_id);
                assert!(paths.contains(&path_a));
                assert!(paths.contains(&path_b));
            }
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

    #[test]
    fn find_for_attach_skips_non_numeric_directory_entries() {
        // Defensive: macOS .DS_Store at year/month/day levels must not break
        // the scan. The valid rollout under a real numeric tree still resolves.
        let tmp = TempDir::new().unwrap();
        let sessions = tmp.path().join(".codex").join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        std::fs::write(sessions.join(".DS_Store"), b"junk").unwrap();
        std::fs::create_dir_all(sessions.join("not-a-year")).unwrap();
        let date = NaiveDate::from_ymd_opt(2026, 5, 15).unwrap();
        let session_id = "019e2c5f-dddd-7000-8000-000000000004";
        let target = write_rollout(tmp.path(), date, session_id);

        let (path, parsed_date) =
            find_codex_session_file_for_attach(tmp.path(), session_id).unwrap();
        assert_eq!(path, target);
        assert_eq!(parsed_date, date);
    }

    /// Test sleeper that records each requested sleep duration without
    /// actually sleeping. Lets enrichment retry tests run instantly.
    struct RecordingSleeper(Mutex<Vec<Duration>>);

    impl RecordingSleeper {
        fn new() -> Self {
            Self(Mutex::new(Vec::new()))
        }

        fn recorded(&self) -> Vec<Duration> {
            self.0.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl Sleeper for RecordingSleeper {
        async fn sleep(&self, duration: Duration) {
            self.0.lock().unwrap().push(duration);
        }
    }

    #[tokio::test]
    async fn load_with_retry_returns_default_after_all_attempts_miss() {
        // File never appears — three attempts, two inter-attempt sleeps,
        // total 400ms worst case before default.
        let tmp = TempDir::new().unwrap();
        let date = NaiveDate::from_ymd_opt(2026, 5, 15).unwrap();
        let sleeper = RecordingSleeper::new();

        let result = load_with_retry(tmp.path(), date, "no-such-session", &sleeper).await;
        assert_eq!(result, Enrichment::default());

        let sleeps = sleeper.recorded();
        assert_eq!(
            sleeps.len(),
            2,
            "two backoffs between three attempts on all-miss"
        );
        for sleep in &sleeps {
            assert_eq!(*sleep, Duration::from_millis(ENRICHMENT_RETRY_DELAY_MS));
        }
    }

    #[tokio::test]
    async fn load_with_retry_succeeds_on_first_attempt_with_zero_sleeps() {
        // Codex writes synchronously, so the file is normally already on
        // disk. This pins the "typical case pays zero latency" contract —
        // a regression that re-introduced a pre-attempt sleep would
        // surface here as a non-empty recorded list.
        let tmp = TempDir::new().unwrap();
        let date = NaiveDate::from_ymd_opt(2026, 5, 15).unwrap();
        let session_id = "019e2c5f-aaaa-7000-8000-000000000001";
        let dir = session_directory(tmp.path(), date);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(format!("rollout-1-{session_id}.jsonl")),
            r#"{"type":"session_meta","payload":{"cli_version":"0.130.0"}}"#,
        )
        .unwrap();
        let sleeper = RecordingSleeper::new();

        let result = load_with_retry(tmp.path(), date, session_id, &sleeper).await;
        assert_eq!(result.cli_version.as_deref(), Some("0.130.0"));
        assert!(
            sleeper.recorded().is_empty(),
            "first-attempt success pays zero latency"
        );
    }

    /// Sleeper that materializes a target file on its first `sleep` call —
    /// simulates "writer was mid-flush during attempt 1, flushed by
    /// attempt 2." Records each requested duration like
    /// [`RecordingSleeper`] for assertion.
    struct StagingSleeper {
        target: PathBuf,
        content: String,
        recorded: Mutex<Vec<Duration>>,
    }

    #[async_trait::async_trait]
    impl Sleeper for StagingSleeper {
        async fn sleep(&self, duration: Duration) {
            self.recorded.lock().unwrap().push(duration);
            if !self.target.exists() {
                std::fs::write(&self.target, &self.content).unwrap();
            }
        }
    }

    #[tokio::test]
    async fn load_with_retry_succeeds_on_second_attempt_with_one_sleep() {
        // The retry exists to defend against filesystem-flush latency on
        // slow disks — file absent on the first try, present by the
        // second. One backoff before success.
        let tmp = TempDir::new().unwrap();
        let date = NaiveDate::from_ymd_opt(2026, 5, 15).unwrap();
        let session_id = "019e2c5f-aaaa-7000-8000-000000000002";
        let dir = session_directory(tmp.path(), date);
        std::fs::create_dir_all(&dir).unwrap();
        let sleeper = StagingSleeper {
            target: dir.join(format!("rollout-2-{session_id}.jsonl")),
            content: r#"{"type":"session_meta","payload":{"cli_version":"0.130.1"}}"#.to_owned(),
            recorded: Mutex::new(Vec::new()),
        };

        let result = load_with_retry(tmp.path(), date, session_id, &sleeper).await;
        assert_eq!(result.cli_version.as_deref(), Some("0.130.1"));
        assert_eq!(
            sleeper.recorded.lock().unwrap().len(),
            1,
            "one backoff before second-attempt success"
        );
    }

    #[test]
    fn build_session_meta_fields_returns_none_when_no_model_and_no_version() {
        // Empty enrichment → no SessionMeta worth emitting.
        let result = build_session_meta_fields(&Enrichment::default(), vec![], vec![]);
        assert!(result.is_none());
    }

    #[test]
    fn build_session_meta_fields_returns_some_when_cli_version_present() {
        let e = Enrichment {
            cli_version: Some("0.130.0".to_owned()),
            ..Default::default()
        };
        let result = build_session_meta_fields(&e, vec![], vec![]);
        let fields = result.expect("Some");
        assert_eq!(fields.harness_version, "0.130.0");
        assert_eq!(fields.model, "", "missing model becomes empty string");
    }

    #[test]
    fn build_session_meta_fields_returns_some_when_model_present() {
        let e = Enrichment {
            model: Some("gpt-5.5".to_owned()),
            ..Default::default()
        };
        let result = build_session_meta_fields(&e, vec![], vec![]);
        assert!(result.is_some());
    }

    #[test]
    fn load_codex_transcript_with_no_partition_date_returns_meta_only_empty() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let agent_id = Uuid::now_v7();
        let result =
            load_codex_transcript(home.path(), cwd.path(), "any-session", None, agent_id).unwrap();
        assert!(result.turns.is_empty());
        assert!(result.warnings.is_empty());
        // meta is populated from config loaders (empty here since no config files).
        assert!(result.meta.is_some());
    }

    #[test]
    fn load_codex_transcript_with_missing_file_emits_stale_sidecar_warning() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let agent_id = Uuid::now_v7();
        let date = NaiveDate::from_ymd_opt(2026, 5, 14).unwrap();
        let result = load_codex_transcript(
            home.path(),
            cwd.path(),
            "no-such-session-id",
            Some(date),
            agent_id,
        )
        .unwrap();
        assert!(result.turns.is_empty());
        assert_eq!(result.warnings.len(), 1);
        assert_eq!(
            result.warnings[0].reason,
            "session file no longer at recorded path"
        );
    }

    fn write_session_at(home: &Path, date: NaiveDate, session_id: &str, content: &str) -> PathBuf {
        let dir = session_directory(home, date);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("rollout-2026-{session_id}.jsonl"));
        std::fs::write(&path, content).unwrap();
        path
    }

    fn jsonl_lines(records: &[Value]) -> String {
        records
            .iter()
            .map(|r| serde_json::to_string(r).unwrap())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn task_started(turn_id: &str, ts: &str, window: u64) -> Value {
        serde_json::json!({
            "timestamp": ts,
            "type": "event_msg",
            "payload": {
                "type": "task_started",
                "turn_id": turn_id,
                "model_context_window": window
            }
        })
    }

    fn user_message(text: &str, ts: &str) -> Value {
        serde_json::json!({
            "timestamp": ts,
            "type": "event_msg",
            "payload": { "type": "user_message", "message": text }
        })
    }

    fn agent_message(text: &str, ts: &str) -> Value {
        serde_json::json!({
            "timestamp": ts,
            "type": "event_msg",
            "payload": { "type": "agent_message", "message": text }
        })
    }

    fn task_complete(turn_id: &str, ts: &str) -> Value {
        serde_json::json!({
            "timestamp": ts,
            "type": "event_msg",
            "payload": { "type": "task_complete", "turn_id": turn_id }
        })
    }

    fn turn_context(model: &str, ts: &str) -> Value {
        serde_json::json!({
            "timestamp": ts,
            "type": "turn_context",
            "payload": { "model": model }
        })
    }

    #[test]
    fn load_codex_transcript_text_only_turn_produces_user_and_agent() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let agent_id = Uuid::now_v7();
        let date = NaiveDate::from_ymd_opt(2026, 5, 14).unwrap();
        let session_id = "019e27fa-ae19-7022-97a2-356e6e5f3360";
        let content = jsonl_lines(&[
            task_started(session_id, "2026-05-14T19:33:20Z", 258_400),
            turn_context("gpt-5.4", "2026-05-14T19:33:20Z"),
            user_message("hi", "2026-05-14T19:33:21Z"),
            agent_message("hello", "2026-05-14T19:33:22Z"),
            task_complete(session_id, "2026-05-14T19:33:23Z"),
        ]);
        write_session_at(home.path(), date, session_id, &content);

        let result =
            load_codex_transcript(home.path(), cwd.path(), session_id, Some(date), agent_id)
                .unwrap();

        assert_eq!(result.turns.len(), 2);
        assert!(matches!(&result.turns[0], Turn::User { text, .. } if text == "hi"));
        match &result.turns[1] {
            Turn::Agent { items, status, .. } => {
                assert!(matches!(status, TurnStatus::Complete));
                assert_eq!(items.len(), 1);
                assert!(matches!(&items[0], TurnItem::Text { text, .. } if text == "hello"));
            }
            _ => panic!("expected Agent turn"),
        }
        let meta = result.meta.unwrap();
        assert_eq!(meta.model, "gpt-5.4");
    }

    #[test]
    fn load_codex_transcript_function_call_pairs_with_output() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let agent_id = Uuid::now_v7();
        let date = NaiveDate::from_ymd_opt(2026, 5, 14).unwrap();
        let session_id = "019e27fa-ae19-7022-97a2-356e6e5f3361";
        let function_call = serde_json::json!({
            "timestamp": "2026-05-14T19:33:22Z",
            "type": "response_item",
            "payload": {
                "type": "function_call",
                "name": "exec_command",
                "call_id": "call_xyz",
                "arguments": r#"{"cmd":"ls"}"#
            }
        });
        let function_call_output = serde_json::json!({
            "timestamp": "2026-05-14T19:33:23Z",
            "type": "response_item",
            "payload": {
                "type": "function_call_output",
                "call_id": "call_xyz",
                "output": "stdout: ok"
            }
        });
        let content = jsonl_lines(&[
            task_started(session_id, "2026-05-14T19:33:20Z", 258_400),
            turn_context("gpt-5.4", "2026-05-14T19:33:20Z"),
            user_message("run", "2026-05-14T19:33:21Z"),
            function_call,
            function_call_output,
            task_complete(session_id, "2026-05-14T19:33:24Z"),
        ]);
        write_session_at(home.path(), date, session_id, &content);

        let result =
            load_codex_transcript(home.path(), cwd.path(), session_id, Some(date), agent_id)
                .unwrap();
        let Turn::Agent { items, .. } = &result.turns[1] else {
            panic!("expected Agent turn");
        };
        assert_eq!(items.len(), 1);
        match &items[0] {
            TurnItem::Tool {
                tool_use_id,
                kind,
                name,
                output,
                ..
            } => {
                assert_eq!(tool_use_id, "call_xyz");
                assert_eq!(*kind, ToolKind::Builtin);
                assert_eq!(name, "exec_command");
                assert_eq!(output.as_deref(), Some("stdout: ok"));
            }
            _ => panic!("expected Tool item"),
        }
    }

    #[test]
    fn load_codex_transcript_function_call_with_mcp_namespace_classifies_as_mcp() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let agent_id = Uuid::now_v7();
        let date = NaiveDate::from_ymd_opt(2026, 5, 14).unwrap();
        let session_id = "019e27fa-ae19-7022-97a2-356e6e5f3362";
        let function_call = serde_json::json!({
            "timestamp": "2026-05-14T19:33:22Z",
            "type": "response_item",
            "payload": {
                "type": "function_call",
                "name": "create_note",
                "namespace": "mcp__tiddly_notes_bookmarks__",
                "call_id": "call_mcp1",
                "arguments": "{}"
            }
        });
        let mcp_end = serde_json::json!({
            "timestamp": "2026-05-14T19:33:23Z",
            "type": "event_msg",
            "payload": {
                "type": "mcp_tool_call_end",
                "call_id": "call_mcp1",
                "invocation": { "server": "tiddly_notes_bookmarks", "tool": "create_note" },
                "result": { "Ok": { "content": [{"type":"text","text":"ok"}], "isError": false } }
            }
        });
        let content = jsonl_lines(&[
            task_started(session_id, "2026-05-14T19:33:20Z", 258_400),
            turn_context("gpt-5.4", "2026-05-14T19:33:20Z"),
            user_message("mcp call", "2026-05-14T19:33:21Z"),
            function_call,
            mcp_end,
            task_complete(session_id, "2026-05-14T19:33:24Z"),
        ]);
        write_session_at(home.path(), date, session_id, &content);

        let result =
            load_codex_transcript(home.path(), cwd.path(), session_id, Some(date), agent_id)
                .unwrap();
        let Turn::Agent { items, .. } = &result.turns[1] else {
            panic!("expected Agent turn");
        };
        match &items[0] {
            TurnItem::Tool {
                kind,
                name,
                output,
                is_error,
                ..
            } => {
                assert_eq!(*kind, ToolKind::Mcp);
                assert_eq!(name, "tiddly_notes_bookmarks.create_note");
                assert_eq!(output.as_deref(), Some("ok"));
                assert_eq!(*is_error, Some(false));
            }
            _ => panic!("expected Tool item"),
        }
    }

    #[test]
    fn load_codex_transcript_token_count_populates_usage() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let agent_id = Uuid::now_v7();
        let date = NaiveDate::from_ymd_opt(2026, 5, 14).unwrap();
        let session_id = "019e27fa-ae19-7022-97a2-356e6e5f3363";
        let token_count = serde_json::json!({
            "timestamp": "2026-05-14T19:33:23Z",
            "type": "event_msg",
            "payload": {
                "type": "token_count",
                "info": {
                    "last_token_usage": {
                        "input_tokens": 100,
                        "output_tokens": 50,
                        "cached_input_tokens": 20,
                        "reasoning_output_tokens": 5
                    }
                }
            }
        });
        let content = jsonl_lines(&[
            task_started(session_id, "2026-05-14T19:33:20Z", 258_400),
            turn_context("gpt-5.4", "2026-05-14T19:33:20Z"),
            user_message("hi", "2026-05-14T19:33:21Z"),
            agent_message("hello", "2026-05-14T19:33:22Z"),
            token_count,
            task_complete(session_id, "2026-05-14T19:33:24Z"),
        ]);
        write_session_at(home.path(), date, session_id, &content);

        let result =
            load_codex_transcript(home.path(), cwd.path(), session_id, Some(date), agent_id)
                .unwrap();
        let Turn::Agent { usage, .. } = &result.turns[1] else {
            panic!("expected Agent turn");
        };
        let usage = usage.as_ref().expect("usage populated");
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 50);
        assert_eq!(usage.cached_input_tokens, Some(20));
        assert_eq!(usage.context_window, Some(258_400));
    }

    #[test]
    fn load_codex_transcript_truncated_mid_turn_marks_failed() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let agent_id = Uuid::now_v7();
        let date = NaiveDate::from_ymd_opt(2026, 5, 14).unwrap();
        let session_id = "019e27fa-ae19-7022-97a2-356e6e5f3364";
        let content = jsonl_lines(&[
            task_started(session_id, "2026-05-14T19:33:20Z", 258_400),
            turn_context("gpt-5.4", "2026-05-14T19:33:20Z"),
            user_message("hi", "2026-05-14T19:33:21Z"),
            agent_message("hello", "2026-05-14T19:33:22Z"),
            // No task_complete — truncated.
        ]);
        write_session_at(home.path(), date, session_id, &content);

        let result =
            load_codex_transcript(home.path(), cwd.path(), session_id, Some(date), agent_id)
                .unwrap();
        let Turn::Agent { status, .. } = &result.turns[1] else {
            panic!("expected Agent turn");
        };
        assert!(matches!(status, TurnStatus::Failed));
    }

    #[test]
    fn load_codex_transcript_malformed_line_is_skipped_with_warning() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let agent_id = Uuid::now_v7();
        let date = NaiveDate::from_ymd_opt(2026, 5, 14).unwrap();
        let session_id = "019e27fa-ae19-7022-97a2-356e6e5f3365";
        let content = format!(
            "{}\n{{ not valid\n{}\n{}",
            serde_json::to_string(&task_started(session_id, "2026-05-14T19:33:20Z", 258_400))
                .unwrap(),
            serde_json::to_string(&agent_message("hello", "2026-05-14T19:33:22Z")).unwrap(),
            serde_json::to_string(&task_complete(session_id, "2026-05-14T19:33:23Z")).unwrap(),
        );
        write_session_at(home.path(), date, session_id, &content);

        let result =
            load_codex_transcript(home.path(), cwd.path(), session_id, Some(date), agent_id)
                .unwrap();
        assert!(!result.warnings.is_empty(), "warning emitted for bad line");
        assert_eq!(result.warnings[0].line_number, 2);
    }

    #[test]
    fn load_codex_transcript_propagates_rate_limits_to_last_rate_limit() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let agent_id = Uuid::now_v7();
        let date = NaiveDate::from_ymd_opt(2026, 5, 14).unwrap();
        let session_id = "019e27fa-ae19-7022-97a2-356e6e5f3366";
        let rate_limit_record = serde_json::json!({
            "timestamp": "2026-05-14T19:33:23Z",
            "type": "event_msg",
            "payload": {
                "type": "token_count",
                "info": null,
                "rate_limits": { "primary": { "used_percent": 10.0 } }
            }
        });
        let content = jsonl_lines(&[
            task_started(session_id, "2026-05-14T19:33:20Z", 258_400),
            turn_context("gpt-5.4", "2026-05-14T19:33:20Z"),
            user_message("hi", "2026-05-14T19:33:21Z"),
            agent_message("ok", "2026-05-14T19:33:22Z"),
            rate_limit_record,
            task_complete(session_id, "2026-05-14T19:33:24Z"),
        ]);
        write_session_at(home.path(), date, session_id, &content);

        let result =
            load_codex_transcript(home.path(), cwd.path(), session_id, Some(date), agent_id)
                .unwrap();
        let rl = result.last_rate_limit.unwrap();
        assert_eq!(rl["primary"]["used_percent"].as_f64(), Some(10.0));
    }

    #[test]
    fn load_codex_transcript_ignores_response_item_message_uses_agent_message() {
        // Pin the canonical text source: even when a session file ALSO
        // carries a `response_item/message` record with the structured
        // model-API content, we extract the agent's text from
        // `event_msg/agent_message`. Consuming both would double-count;
        // this test fails loud if a future change parses `response_item/
        // message` as a fallback.
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let agent_id = Uuid::now_v7();
        let date = NaiveDate::from_ymd_opt(2026, 5, 14).unwrap();
        let session_id = "019e27fa-ae19-7022-97a2-356e6e5f3367";
        let response_item_message = serde_json::json!({
            "timestamp": "2026-05-14T19:33:22Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "assistant",
                "content": [{ "type": "output_text", "text": "should-be-ignored" }]
            }
        });
        let content = jsonl_lines(&[
            task_started(session_id, "2026-05-14T19:33:20Z", 258_400),
            turn_context("gpt-5.4", "2026-05-14T19:33:20Z"),
            user_message("hi", "2026-05-14T19:33:21Z"),
            response_item_message,
            agent_message("from-event-msg", "2026-05-14T19:33:22Z"),
            task_complete(session_id, "2026-05-14T19:33:24Z"),
        ]);
        write_session_at(home.path(), date, session_id, &content);

        let result =
            load_codex_transcript(home.path(), cwd.path(), session_id, Some(date), agent_id)
                .unwrap();
        let Turn::Agent { items, .. } = &result.turns[1] else {
            panic!("expected Agent turn");
        };
        assert_eq!(items.len(), 1, "exactly one text item — no duplication");
        match &items[0] {
            TurnItem::Text { text, .. } => {
                assert_eq!(text, "from-event-msg");
            }
            _ => panic!("expected Text item"),
        }
    }
}
