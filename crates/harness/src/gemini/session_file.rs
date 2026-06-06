//! Gemini session-file path helpers and transcript hydration.
//!
//! Gemini stores session files under
//! `~/.gemini/tmp/<project-name>/chats/session-<YYYY-MM-DDTHH-MM>-<id8>.jsonl`.
//! `<project-name>` is recorded in `~/.gemini/projects.json` (cwd → name
//! mapping, populated on first headless dispatch). `<id8>` is the first 8
//! hex characters of the session UUID — only 32 bits of disambiguation, so
//! the helpers exposed here return a *candidate set*; callers verify by
//! reading the header `sessionId` field when collision-safety is required.
//!
//! **What this module owns.**
//! - Cwd → project-name lookup (`resolve_gemini_project_name`).
//! - First-8-char-prefix glob over the chats directory
//!   (`gemini_session_file_candidates`).
//! - A single-file existence check used by `build_args` to pick
//!   `--session-id` vs `--resume` (`session_file_exists_for`).
//! - Transcript hydration (`load_gemini_transcript`) with two-layer
//!   collision defense: path-layer demix for separate files sharing the
//!   8-char prefix, content-layer ambiguity warning for files that
//!   accumulated headers from more than one session.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde_json::Value;
use switchboard_core::AgentId;
use uuid::Uuid;

use crate::events::{ContentKind, ToolKind, TurnId, TurnUsage};
use crate::gemini::config::load_mcp_servers;
use crate::gemini::parser::GEMINI_INTERNAL_TOOL_NAMES;
use crate::gemini::skills::load_skills;
use crate::transcript::{
    LoadTranscriptError, LoadedTranscript, ParseWarning, SessionMetaInfo, Turn, TurnItem,
    TurnStatus, merge_meta_with_loaders,
};

/// Take the first 8 hex chars of a UUID as Gemini's filename suffix would.
/// Lowercase (Gemini emits lowercase hex in filenames).
#[must_use]
pub fn id_prefix(session_id: &Uuid) -> String {
    // `simple()` is 32 hex chars, no dashes. Slice the first 8.
    let simple = session_id.simple().to_string();
    simple[..8].to_owned()
}

/// Resolve cwd → Gemini project-name via `~/.gemini/projects.json`.
/// Returns `None` if the file is missing, unreadable, the cwd doesn't
/// exist on disk, or `projects.json` contains no entry for the canonical
/// cwd. Degrading to `None` lets the caller treat "never-dispatched-yet"
/// identically to "lookup failed" — both produce an empty candidate set.
///
/// **Cwd is canonicalized internally** (matches Claude's `session_exists_in`
/// pattern). Gemini's `projects.json` is keyed by its own resolved working
/// directory — on macOS, `/tmp/X` becomes `/private/tmp/X`. Without
/// canonicalization, a non-canonical caller cwd would miss an existing
/// entry, `build_args` would pick `--session-id` on the second turn, and
/// Gemini would exit 42 because the session already exists. The
/// production caller (`Directory::at`) canonicalizes already; this is
/// defensive parity.
#[must_use]
pub fn resolve_gemini_project_name(home_dir: &Path, cwd: &Path) -> Option<String> {
    let canonical = cwd.canonicalize().ok()?;
    let path = home_dir.join(".gemini").join("projects.json");
    let bytes = std::fs::read(&path).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let cwd_str = canonical.to_str()?;
    // The observed file shape is `{"projects": {"<abs-cwd>": "<name>"}}`.
    // The "projects" wrapper key isn't guaranteed across Gemini CLI versions —
    // try both shapes (wrapped + flat) so we degrade gracefully.
    let map = value
        .get("projects")
        .and_then(serde_json::Value::as_object)
        .or_else(|| value.as_object())?;
    map.get(cwd_str)
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

/// Path to Gemini's chats directory for `<project-name>`. Does not check
/// existence; callers glob inside it.
fn chats_dir(home_dir: &Path, project_name: &str) -> PathBuf {
    home_dir
        .join(".gemini")
        .join("tmp")
        .join(project_name)
        .join("chats")
}

/// Enumerate session-file candidates matching `session-*-<id8>.jsonl` in
/// the project's chats directory. Returns an empty vector if the cwd
/// doesn't exist, the project mapping is unknown, or the chats directory
/// is missing or unreadable. Cwd is canonicalized internally via
/// `resolve_gemini_project_name`. Used by:
/// - `session_file_exists_for` (pick `--session-id` vs `--resume`).
/// - The attach lookup (pick the right file when filenames collide).
/// - The transcript hydrator (full session disambiguation by
///   `sessionId` happens at the loader layer).
#[must_use]
pub fn gemini_session_file_candidates(
    home_dir: &Path,
    cwd: &Path,
    session_id: &Uuid,
) -> Vec<PathBuf> {
    let Some(project_name) = resolve_gemini_project_name(home_dir, cwd) else {
        return Vec::new();
    };
    let dir = chats_dir(home_dir, &project_name);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let suffix = format!("-{}.jsonl", id_prefix(session_id));
    let prefix = "session-";
    let mut hits = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if name.starts_with(prefix) && name.ends_with(&suffix) {
            hits.push(path);
        }
    }
    hits
}

/// True if at least one session file matches the prefix for `session_id` in
/// the project's chats directory. Used by `build_args` to decide between
/// `--session-id` (first turn) and `--resume` (subsequent turns), mirroring
/// the Claude Code pattern.
///
/// **Why prefix-only is correct here**: under Switchboard's UUID-v4 policy
/// for Gemini session IDs, the first-8-char collision probability is
/// ~1/2^32. Existence-by-prefix is effectively existence-by-full-id. A
/// future cross-collision under an external test fixture would mis-route a
/// first turn as a resume — handled by `--resume <unknown-uuid>` failing
/// with exit 42, surfaced as `AdapterFailure`. Acceptable trade-off
/// given the probability.
#[must_use]
pub fn session_file_exists_for(home_dir: &Path, cwd: &Path, session_id: &Uuid) -> bool {
    !gemini_session_file_candidates(home_dir, cwd, session_id).is_empty()
}

/// Load a Gemini session file for `session_id` and project a
/// `LoadedTranscript`. Mirrors `load_claude_transcript` in shape; Gemini
/// follows the Claude pattern (caller-controlled session ID, file on disk
/// keyed by session UUID), not the Codex pattern (sidecar).
///
/// Returns `Ok(LoadedTranscript::default())` when:
/// - `projects.json` doesn't list this cwd (agent never dispatched).
/// - No session file matches the 8-char prefix in the project's chats
///   directory.
/// - The candidate set is non-empty but none of the candidates' first
///   record matches the full target UUID (filename-level prefix collision
///   resolved at the path layer).
///
/// Returns `Ok(LoadedTranscript { turns: vec![], warnings: [collision] })`
/// when the matched file contains multiple distinct `kind:"main"` session
/// headers (intra-file prefix collision — two sessions appended to the
/// same file with colliding 8-char prefixes in the same minute). Under
/// UUID v4 (the Gemini policy), the probability is ~1/2^32; the defense
/// exists so the rare case fails loudly instead of silently merging
/// transcripts.
///
/// `home_dir` is injected for testability.
pub fn load_gemini_transcript(
    home_dir: &Path,
    cwd: &Path,
    session_id: Uuid,
    agent_id: AgentId,
) -> Result<LoadedTranscript, LoadTranscriptError> {
    let mut candidates = gemini_session_file_candidates(home_dir, cwd, &session_id);
    if candidates.is_empty() {
        // Never-dispatched-yet case — still surface MCP + skills so the
        // sidebar populates the moment the agent is selected, matching
        // Claude / Codex hydration shape.
        return Ok(LoadedTranscript {
            meta: Some(merge_meta_with_loaders(
                None,
                load_mcp_servers(home_dir, cwd),
                load_skills(home_dir, cwd),
            )),
            ..LoadedTranscript::default()
        });
    }
    // Sort by filename so the merge below preserves chronology
    // (filename embeds the per-invocation timestamp:
    // `session-<YYYY-MM-DDTHH-MM>-<id8>.jsonl`).
    candidates.sort();

    // **Why we merge content across all matching files**: Gemini creates
    // a new session file on each `--resume` invocation (a separate file
    // per dispatch, distinct timestamp in the filename). Empirically, a
    // multi-turn session ends up with one file holding the full
    // conversation history and several near-empty stub files (just a
    // header) representing later resume invocations that didn't append
    // new content. Returning the first Unambiguous match (the previous
    // behavior) is filesystem-iteration-order dependent and silently
    // drops the conversation on a coin flip.
    //
    // We rely on the observed pattern: only one matching file carries
    // conversation records; the others contribute nothing under parsing.
    // The parser dedupes gemini-records by id within an agent window
    // (last-wins) and skips header records, which makes this safe for
    // the observed pattern. The parser does **not** dedupe user-records
    // across files — if Gemini ever writes the same user turn to two
    // matching files, hydration will produce duplicate `Turn::User`
    // entries (and the following gemini-records will attach to whichever
    // user-record was parsed last). Audit the failure mode if the
    // resume-creates-stub-file pattern ever changes; if it does, capture
    // a fixture and add user-record dedup keyed on the on-disk record id.
    // Consequence downstream: an attached Gemini session whose imported
    // history hit this duplicate path would render that prompt twice in the
    // unified transcript (the conversation merge surfaces pre-journaling user
    // turns verbatim). Cosmetic, not data loss; fixed here (dedup) rather than
    // in the merge if it ever materializes.
    //
    // Ambiguity in any single candidate aborts the whole merge — even
    // when prior candidates were clean. The clean files' content is
    // discarded; the user sees the ambiguity warning + empty turns.
    // Rationale: an ambiguous file (one file, multiple distinct
    // sessions) means a different session wrote into this UUID's
    // filename-prefix namespace. The clean files might still be
    // correctly attributed, but we no longer trust our enumeration of
    // "which files belong to this session" — surfacing partial content
    // under an ambiguity warning would mislead the user about what's
    // reliable. Under UUID v4 the probability is ~1/2^32 ×
    // resume-invocation count, so the conservative bail rarely loses
    // anything real.
    let mut merged = String::new();
    for path in &candidates {
        let content = std::fs::read_to_string(path).map_err(|e| LoadTranscriptError::Io {
            path: path.clone(),
            source: e,
        })?;
        match classify_candidate(&content, session_id) {
            CandidateMatch::NoTarget => {}
            CandidateMatch::Ambiguous => return Ok(ambiguous_session_warning()),
            CandidateMatch::Unambiguous => {
                merged.push_str(&content);
                if !merged.ends_with('\n') {
                    merged.push('\n');
                }
            }
        }
    }

    if merged.is_empty() {
        // Candidates existed but none matched the target — same "fresh"
        // outcome as the empty-candidates case; still surface registries.
        return Ok(LoadedTranscript {
            meta: Some(merge_meta_with_loaders(
                None,
                load_mcp_servers(home_dir, cwd),
                load_skills(home_dir, cwd),
            )),
            ..LoadedTranscript::default()
        });
    }

    let mut transcript = parse_gemini_transcript_content(&merged, agent_id);
    transcript.meta = Some(merge_meta_with_loaders(
        transcript.meta.take(),
        load_mcp_servers(home_dir, cwd),
        load_skills(home_dir, cwd),
    ));
    Ok(transcript)
}

/// Outcome of scanning one candidate file's `kind:"main"` headers against
/// the requested target session. Public so the attach flow can reuse the
/// same disambiguation rule as transcript hydration.
#[derive(Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum CandidateMatch {
    /// No header in the file matches the target. Try the next candidate.
    NoTarget,
    /// The target is present in this file *and* the file contains records
    /// from more than one session (more than one distinct header
    /// `sessionId` observed). Cannot safely demix from file content alone;
    /// the loader surfaces an ambiguity warning and the attach flow
    /// rejects with `AmbiguousSessionFile`.
    Ambiguous,
    /// The target is the file's only session. Safe to hydrate / attach.
    Unambiguous,
}

/// Walk `content` line-by-line and classify the file against `target`.
/// Malformed JSON lines and lines without `kind:"main"` are skipped
/// silently — only valid `main` headers with a parseable `sessionId`
/// contribute to the distinct-session count.
#[must_use]
pub fn classify_candidate(content: &str, target: Uuid) -> CandidateMatch {
    let mut distinct: Vec<Uuid> = Vec::new();
    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if value.get("kind").and_then(Value::as_str) != Some("main") {
            continue;
        }
        let Some(sid_str) = value.get("sessionId").and_then(Value::as_str) else {
            continue;
        };
        let Ok(sid) = Uuid::parse_str(sid_str) else {
            continue;
        };
        if !distinct.contains(&sid) {
            distinct.push(sid);
        }
    }
    let contains_target = distinct.contains(&target);
    match (contains_target, distinct.len()) {
        (false, _) => CandidateMatch::NoTarget,
        (true, 1) => CandidateMatch::Unambiguous,
        (true, _) => CandidateMatch::Ambiguous,
    }
}

fn ambiguous_session_warning() -> LoadedTranscript {
    LoadedTranscript {
        turns: Vec::new(),
        meta: None,
        last_rate_limit: None,
        last_rate_limit_as_of: None,
        warnings: vec![ParseWarning {
            line_number: 0,
            reason:
                "session-file contains records from multiple sessions; ambiguous, transcript not hydrated"
                    .to_owned(),
        }],
    }
}

/// Parse Gemini session-file content into a `LoadedTranscript` (no FS
/// access). The caller guarantees the file has been gated for
/// ambiguity / target-session match by `classify_candidate`, so this
/// function is pure reconstruction — it does not enforce any session
/// invariant. Exposed `pub(crate)` for tests that want to drive
/// reconstruction without staging a temp file.
pub(crate) fn parse_gemini_transcript_content(
    content: &str,
    agent_id: AgentId,
) -> LoadedTranscript {
    let mut state = GeminiReconstruction::new(agent_id);
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
    state.finalize()
}

struct GeminiReconstruction {
    agent_id: AgentId,
    turns: Vec<Turn>,
    current_agent: Option<GeminiAgentBuilder>,
    warnings: Vec<ParseWarning>,
    model: Option<String>,
}

struct GeminiAgentBuilder {
    turn_id: TurnId,
    agent_id: AgentId,
    started_at: DateTime<Utc>,
    last_seen_at: DateTime<Utc>,
    records: Vec<GeminiRecord>,
    last_usage: Option<TurnUsage>,
    last_model: Option<String>,
}

/// A buffered gemini-record in the current turn window. Records sharing
/// `id` are deduped last-wins (Gemini accrues data into a record across
/// multiple writes; the final copy is the complete one). Buffer preserves
/// insertion order so emitted items follow the file's chronology.
#[derive(Clone)]
struct GeminiRecord {
    id: String,
    content: String,
    tool_calls: Vec<GeminiToolCall>,
    usage: Option<TurnUsage>,
    model: Option<String>,
}

#[derive(Clone)]
struct GeminiToolCall {
    id: String,
    name: String,
    args: Value,
    output: Option<String>,
    is_error: bool,
    timestamp: Option<DateTime<Utc>>,
}

impl GeminiReconstruction {
    fn new(agent_id: AgentId) -> Self {
        Self {
            agent_id,
            turns: Vec::new(),
            current_agent: None,
            warnings: Vec::new(),
            model: None,
        }
    }

    fn warn(&mut self, line_number: usize, reason: impl Into<String>) {
        self.warnings.push(ParseWarning {
            line_number,
            reason: reason.into(),
        });
    }

    fn ingest(&mut self, line_number: usize, record: &Value) {
        if record.get("$set").is_some() {
            return;
        }
        // Header records carry only session metadata, not transcript
        // content. Ambiguity / target-session checks live at the loader
        // (`classify_candidate`); reconstruction skips headers.
        if record.get("kind").and_then(Value::as_str) == Some("main") {
            return;
        }
        match record.get("type").and_then(Value::as_str) {
            Some("user") => self.handle_user(record),
            Some("gemini") => self.handle_gemini(line_number, record),
            _ => {}
        }
    }

    fn handle_user(&mut self, record: &Value) {
        let text = record
            .get("content")
            .and_then(Value::as_array)
            .and_then(|arr| arr.first())
            .and_then(|item| item.get("text"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let started_at = parse_timestamp(record).unwrap_or_else(Utc::now);
        self.close_current_agent(TurnStatus::Complete);
        self.turns.push(Turn::User {
            turn_id: Uuid::now_v7(),
            agent_id: self.agent_id,
            started_at,
            text,
        });
    }

    fn handle_gemini(&mut self, line_number: usize, record: &Value) {
        let Some(id) = record.get("id").and_then(Value::as_str) else {
            self.warn(line_number, "gemini record missing id; skipped");
            return;
        };
        let timestamp = parse_timestamp(record);
        let started_at = timestamp.unwrap_or_else(Utc::now);
        let builder = self
            .current_agent
            .get_or_insert_with(|| GeminiAgentBuilder {
                turn_id: Uuid::now_v7(),
                agent_id: self.agent_id,
                started_at,
                last_seen_at: started_at,
                records: Vec::new(),
                last_usage: None,
                last_model: None,
            });
        if let Some(t) = timestamp {
            builder.last_seen_at = t;
        }

        let parsed = parse_gemini_record(id, record);
        if let Some(usage) = parsed.usage.clone() {
            builder.last_usage = Some(usage);
        }
        if let Some(model) = parsed.model.clone() {
            builder.last_model = Some(model);
        }

        if let Some(existing) = builder.records.iter_mut().find(|r| r.id == parsed.id) {
            *existing = parsed;
        } else {
            builder.records.push(parsed);
        }
    }

    fn close_current_agent(&mut self, status: TurnStatus) {
        let Some(builder) = self.current_agent.take() else {
            return;
        };
        let mut items: Vec<TurnItem> = Vec::new();
        for record in &builder.records {
            if !record.content.is_empty() {
                items.push(TurnItem::Text {
                    kind: ContentKind::Text,
                    text: record.content.clone(),
                });
            }
            // Gemini's reasoning (`thoughts`) is intentionally NOT surfaced: it
            // is written only to the session file, never to the live stream, so
            // rendering it would show after-the-fact reasoning that only appears
            // on reopen — stale UX. See docs/research/harness-behavior.md §3.2.
            for tc in &record.tool_calls {
                if GEMINI_INTERNAL_TOOL_NAMES.contains(&tc.name.as_str()) {
                    continue;
                }
                let kind = if tc.name.starts_with("mcp__") {
                    ToolKind::Mcp
                } else {
                    ToolKind::Builtin
                };
                let tc_ts = tc.timestamp.unwrap_or(builder.last_seen_at);
                items.push(TurnItem::Tool {
                    tool_use_id: tc.id.clone(),
                    kind,
                    name: tc.name.clone(),
                    input: tc.args.clone(),
                    output: tc.output.clone(),
                    is_error: Some(tc.is_error),
                    started_at: tc_ts,
                    completed_at: Some(tc_ts),
                });
            }
        }
        if self.model.is_none()
            && let Some(model) = builder.last_model.clone()
        {
            self.model = Some(model);
        }
        self.turns.push(Turn::Agent {
            turn_id: builder.turn_id,
            agent_id: builder.agent_id,
            started_at: builder.started_at,
            ended_at: Some(builder.last_seen_at),
            status,
            items,
            usage: builder.last_usage,
            // Per-turn model from this turn's own `gemini` record(s). Distinct
            // from `self.model` (first-wins, agent-scoped `meta.model`). Gemini
            // has no effort axis.
            model: builder.last_model.clone(),
            effort: None,
            // Gemini has no cost/overage and no join key (Claude-only feature).
            spend: None,
            stable_message_id: None,
        });
    }

    fn finalize(mut self) -> LoadedTranscript {
        self.close_current_agent(TurnStatus::Complete);
        let meta = self.model.clone().map(|model| SessionMetaInfo {
            model,
            harness_version: String::new(),
            tools: vec![],
            mcp_servers: vec![],
            skills: vec![],
        });
        LoadedTranscript {
            turns: self.turns,
            meta,
            last_rate_limit: None,
            last_rate_limit_as_of: None,
            warnings: self.warnings,
        }
    }
}

fn parse_timestamp(record: &Value) -> Option<DateTime<Utc>> {
    record
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc))
}

fn parse_gemini_record(id: &str, record: &Value) -> GeminiRecord {
    let content = record
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let tool_calls = record
        .get("toolCalls")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().map(parse_tool_call).collect())
        .unwrap_or_default();
    let usage = record.get("tokens").map(parse_tokens);
    let model = record
        .get("model")
        .and_then(Value::as_str)
        .map(str::to_owned);
    GeminiRecord {
        id: id.to_owned(),
        content,
        tool_calls,
        usage,
        model,
    }
}

fn parse_tool_call(value: &Value) -> GeminiToolCall {
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let name = value
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let args = value.get("args").cloned().unwrap_or(Value::Null);
    let status = value.get("status").and_then(Value::as_str).unwrap_or("");
    let is_error = !status.is_empty() && status != "success";
    let output = value
        .get("result")
        .and_then(Value::as_array)
        .and_then(|arr| arr.first())
        .and_then(|first| first.get("functionResponse"))
        .and_then(|fr| fr.get("response"))
        .map(extract_response_output);
    let timestamp = parse_timestamp(value);
    GeminiToolCall {
        id,
        name,
        args,
        output,
        is_error,
        timestamp,
    }
}

/// Extract a tool's user-visible output from its `functionResponse.response`.
/// Prefers the `output` field (Gemini's read-like tools shape) but falls
/// back to the entire response object stringified when `output` is absent.
fn extract_response_output(response: &Value) -> String {
    if let Some(output) = response.get("output").and_then(Value::as_str) {
        return output.to_owned();
    }
    serde_json::to_string(response).unwrap_or_default()
}

fn parse_tokens(value: &Value) -> TurnUsage {
    let input_tokens = value.get("input").and_then(Value::as_u64).unwrap_or(0);
    let output_tokens = value.get("output").and_then(Value::as_u64).unwrap_or(0);
    let cached_input_tokens = value.get("cached").and_then(Value::as_u64);
    TurnUsage {
        input_tokens,
        output_tokens,
        cached_input_tokens,
        cache_creation_input_tokens: None,
        // Gemini exposes no context window — occupancy is never computed and
        // the bar stays hidden; leave the reconciled value `None`.
        context_input_tokens: None,
        reasoning_output_tokens: None,
        context_window: None,
        total_cost_usd: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn id_prefix_takes_first_8_hex_chars_lowercase() {
        let uuid = Uuid::parse_str("ABCDEF12-3456-4789-89AB-CDEF01234567").unwrap();
        assert_eq!(id_prefix(&uuid), "abcdef12");
    }

    /// Write `projects.json` mapping the canonical form of `cwd` → name.
    /// Matches what the real Gemini CLI writes (it canonicalizes cwd before
    /// recording the key).
    fn stage_projects_json_wrapped(home: &Path, cwd: &Path, name: &str) {
        let gemini = home.join(".gemini");
        std::fs::create_dir_all(&gemini).unwrap();
        let canonical = cwd.canonicalize().unwrap();
        let body = serde_json::json!({
            "projects": { canonical.to_str().unwrap(): name }
        });
        std::fs::write(gemini.join("projects.json"), body.to_string()).unwrap();
    }

    fn stage_projects_json_flat(home: &Path, cwd: &Path, name: &str) {
        let gemini = home.join(".gemini");
        std::fs::create_dir_all(&gemini).unwrap();
        let canonical = cwd.canonicalize().unwrap();
        let body = serde_json::json!({
            canonical.to_str().unwrap(): name
        });
        std::fs::write(gemini.join("projects.json"), body.to_string()).unwrap();
    }

    #[test]
    fn resolve_returns_none_when_projects_json_missing() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        assert!(resolve_gemini_project_name(home.path(), cwd.path()).is_none());
    }

    #[test]
    fn resolve_returns_none_when_cwd_does_not_exist() {
        // Canonicalize-fail → None. Non-existent cwd is indistinguishable
        // from "never dispatched" at the lookup boundary.
        let home = TempDir::new().unwrap();
        assert!(
            resolve_gemini_project_name(home.path(), Path::new("/definitely/not/a/real/path"))
                .is_none()
        );
    }

    #[test]
    fn resolve_reads_wrapped_projects_map() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        stage_projects_json_wrapped(home.path(), cwd.path(), "my-project");
        assert_eq!(
            resolve_gemini_project_name(home.path(), cwd.path()),
            Some("my-project".to_owned())
        );
    }

    #[test]
    fn resolve_reads_flat_projects_map() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        stage_projects_json_flat(home.path(), cwd.path(), "flat-name");
        assert_eq!(
            resolve_gemini_project_name(home.path(), cwd.path()),
            Some("flat-name".to_owned())
        );
    }

    /// Regression: cwd canonicalization must happen inside the lookup
    /// helper so non-canonical caller paths (symlinks, `/tmp` vs
    /// `/private/tmp` on macOS) still resolve to the canonical key the
    /// Gemini CLI wrote. Without this, second-turn dispatches would
    /// fall through to `--session-id`, Gemini exits 42, agents fail
    /// silently after their first turn.
    #[cfg(unix)]
    #[test]
    fn resolve_canonicalizes_cwd_against_symlinked_path() {
        let home = TempDir::new().unwrap();
        let real = TempDir::new().unwrap();
        let link_parent = TempDir::new().unwrap();
        let link = link_parent.path().join("symlink-to-cwd");
        std::os::unix::fs::symlink(real.path(), &link).unwrap();
        stage_projects_json_wrapped(home.path(), real.path(), "via-canonical");

        assert_eq!(
            resolve_gemini_project_name(home.path(), &link),
            Some("via-canonical".to_owned()),
            "non-canonical cwd must resolve via canonicalize() to the key Gemini wrote"
        );
    }

    #[test]
    fn candidates_empty_when_project_unknown() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let uuid = Uuid::new_v4();
        let hits = gemini_session_file_candidates(home.path(), cwd.path(), &uuid);
        assert!(hits.is_empty());
    }

    #[test]
    fn candidates_match_only_files_with_session_prefix_and_id_suffix() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        stage_projects_json_wrapped(home.path(), cwd.path(), "proj");
        let chats = home
            .path()
            .join(".gemini")
            .join("tmp")
            .join("proj")
            .join("chats");
        std::fs::create_dir_all(&chats).unwrap();

        let uuid = Uuid::parse_str("00000000-0000-4000-8000-000000000001").unwrap();
        let prefix = id_prefix(&uuid);
        let matching = chats.join(format!("session-2026-05-17T22-11-{prefix}.jsonl"));
        let non_matching_suffix = chats.join("session-2026-05-17T22-11-deadbeef.jsonl");
        let non_matching_prefix = chats.join(format!("rollout-other-{prefix}.jsonl"));
        std::fs::write(&matching, "").unwrap();
        std::fs::write(&non_matching_suffix, "").unwrap();
        std::fs::write(&non_matching_prefix, "").unwrap();

        let hits = gemini_session_file_candidates(home.path(), cwd.path(), &uuid);
        assert_eq!(hits, vec![matching]);
    }

    #[test]
    fn session_file_exists_for_picks_up_matching_file() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        stage_projects_json_wrapped(home.path(), cwd.path(), "proj");
        let chats = home
            .path()
            .join(".gemini")
            .join("tmp")
            .join("proj")
            .join("chats");
        std::fs::create_dir_all(&chats).unwrap();

        let uuid = Uuid::parse_str("00000000-0000-4000-8000-000000000001").unwrap();
        let prefix = id_prefix(&uuid);
        std::fs::write(
            chats.join(format!("session-2026-05-17T22-11-{prefix}.jsonl")),
            "",
        )
        .unwrap();

        assert!(session_file_exists_for(home.path(), cwd.path(), &uuid));
        let other = Uuid::new_v4();
        assert!(!session_file_exists_for(home.path(), cwd.path(), &other));
    }

    // -----------------------------------------------------------------
    // Hydrator tests
    // -----------------------------------------------------------------

    const HAPPY_PATH_FIXTURE: &str =
        include_str!("../../tests/fixtures/gemini/happy-path.session.jsonl");
    const TOOL_USE_FIXTURE: &str =
        include_str!("../../tests/fixtures/gemini/tool-use.session.jsonl");
    const INTERLEAVED_FIXTURE: &str =
        include_str!("../../tests/fixtures/gemini/interleaved-collision.session.jsonl");

    fn agent_id() -> AgentId {
        Uuid::now_v7()
    }

    /// Stage a session file under the canonical Gemini layout for `cwd`.
    /// Returns the file path so a caller can assert against it if needed.
    fn stage_session_file(
        home: &Path,
        cwd: &Path,
        project_name: &str,
        filename: &str,
        body: &str,
    ) -> PathBuf {
        stage_projects_json_wrapped(home, cwd, project_name);
        let chats = home
            .join(".gemini")
            .join("tmp")
            .join(project_name)
            .join("chats");
        std::fs::create_dir_all(&chats).unwrap();
        let path = chats.join(filename);
        std::fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn parse_happy_path_fixture_round_trips_user_and_agent_turns() {
        let aid = agent_id();
        let t = parse_gemini_transcript_content(HAPPY_PATH_FIXTURE, aid);

        assert!(
            t.warnings.is_empty(),
            "no warnings expected: {:?}",
            t.warnings
        );
        assert_eq!(t.turns.len(), 2);
        let Turn::User { text, .. } = &t.turns[0] else {
            panic!("expected User turn first, got {:?}", t.turns[0]);
        };
        assert_eq!(text, "Reply with the single word 'ack' and nothing else.");
        let Turn::Agent {
            items,
            usage,
            status,
            ..
        } = &t.turns[1]
        else {
            panic!("expected Agent turn second");
        };
        assert_eq!(*status, TurnStatus::Complete);
        assert_eq!(items.len(), 1);
        let TurnItem::Text { kind, text } = &items[0] else {
            panic!("expected Text item, got {:?}", items[0]);
        };
        assert_eq!(*kind, ContentKind::Text);
        assert_eq!(text, "ack");
        let usage = usage.as_ref().expect("usage from gemini record's tokens");
        assert_eq!(usage.input_tokens, 10178);
        assert_eq!(usage.output_tokens, 1);
        assert_eq!(usage.cached_input_tokens, Some(0));

        let meta = t.meta.as_ref().expect("meta from gemini record's model");
        assert_eq!(meta.model, "gemini-3-flash-preview");
    }

    #[test]
    fn parse_tool_use_fixture_surfaces_real_tool_output_and_filters_update_topic() {
        let t = parse_gemini_transcript_content(TOOL_USE_FIXTURE, agent_id());

        assert!(
            t.warnings.is_empty(),
            "no warnings expected: {:?}",
            t.warnings
        );
        let Turn::Agent { items, .. } = t
            .turns
            .iter()
            .find(|turn| matches!(turn, Turn::Agent { .. }))
            .expect("agent turn present")
        else {
            unreachable!();
        };

        let mut update_topic_seen = false;
        let mut read_file_output: Option<String> = None;
        let mut final_text: Option<String> = None;
        for item in items {
            match item {
                TurnItem::Tool { name, output, .. } => {
                    if name == "update_topic" {
                        update_topic_seen = true;
                    } else if name == "read_file" {
                        read_file_output = output.clone();
                    }
                }
                TurnItem::Text {
                    kind: ContentKind::Text,
                    text,
                } => {
                    final_text = Some(text.clone());
                }
                _ => {}
            }
        }
        assert!(
            !update_topic_seen,
            "update_topic must be filtered from hydrated items"
        );
        assert_eq!(
            read_file_output.as_deref(),
            Some("SWITCHBOARD_GEMINI_PROBE_TOOL_5F8A21\n"),
            "real read_file output from session file must surface (live stream is empty for read-like tools)"
        );
        assert_eq!(
            final_text.as_deref(),
            Some("SWITCHBOARD_GEMINI_PROBE_TOOL_5F8A21"),
            "final assistant text item must be the sentinel"
        );
    }

    #[test]
    fn classify_candidate_recognizes_ambiguous_for_both_targets() {
        // The captured fixture has two distinct headers (...009 and ...00A).
        // Both UUIDs must classify as Ambiguous — the asymmetric path-layer
        // gate that only checked the first header has been replaced with
        // a full header scan.
        let target_a = Uuid::parse_str("00000000-0000-4000-8000-000000000009").unwrap();
        let target_b = Uuid::parse_str("00000000-0000-4000-8000-00000000000A").unwrap();
        assert_eq!(
            classify_candidate(INTERLEAVED_FIXTURE, target_a),
            CandidateMatch::Ambiguous
        );
        assert_eq!(
            classify_candidate(INTERLEAVED_FIXTURE, target_b),
            CandidateMatch::Ambiguous
        );
    }

    #[test]
    fn classify_candidate_returns_unambiguous_for_single_session_file() {
        let target = Uuid::parse_str("00000000-0000-4000-8000-000000000001").unwrap();
        assert_eq!(
            classify_candidate(HAPPY_PATH_FIXTURE, target),
            CandidateMatch::Unambiguous
        );
    }

    #[test]
    fn classify_candidate_returns_no_target_when_target_absent() {
        let unrelated = Uuid::parse_str("00000000-0000-4000-8000-0000000000ff").unwrap();
        assert_eq!(
            classify_candidate(HAPPY_PATH_FIXTURE, unrelated),
            CandidateMatch::NoTarget
        );
    }

    #[test]
    fn classify_candidate_ignores_malformed_header_lines() {
        // Malformed header lines should not contribute to the distinct-
        // session count nor mask a valid target match later in the file.
        let target = Uuid::parse_str("00000000-0000-4000-8000-0000000000cc").unwrap();
        let body = format!(
            r#"not-json-at-all
{{"kind":"main"}}
{{"sessionId":"{target}","kind":"main","startTime":"2026-05-18T00:00:00Z"}}"#
        );
        assert_eq!(
            classify_candidate(&body, target),
            CandidateMatch::Unambiguous
        );
    }

    #[test]
    fn parse_filters_set_mutation_records() {
        // $set lines appear between every real record in the captured
        // fixtures. Direct assertion: a body of nothing but $set lines
        // produces no turns and no warnings.
        let only_sets = r#"{"$set":{"lastUpdated":"2026-05-18T00:00:00Z"}}
{"$set":{"lastUpdated":"2026-05-18T00:00:01Z"}}"#;
        let t = parse_gemini_transcript_content(only_sets, agent_id());
        assert!(t.turns.is_empty());
        assert!(t.warnings.is_empty());
    }

    #[test]
    fn hydrate_stamps_per_turn_model_from_each_record() {
        // Two turns on different models → two agent turns whose `model` differs
        // (verified on disk: a flash→pro switch writes the new model per record).
        // `meta.model` stays first-wins (separate, agent-scoped).
        let session_id = Uuid::parse_str("00000000-0000-4000-8000-0000000000ab").unwrap();
        let body = format!(
            r#"{{"sessionId":"{session_id}","kind":"main","startTime":"2026-05-18T00:00:00Z"}}
{{"id":"u1","timestamp":"2026-05-18T00:00:01Z","type":"user","content":[{{"text":"hi"}}]}}
{{"id":"g1","timestamp":"2026-05-18T00:00:02Z","type":"gemini","content":"a","thoughts":[],"tokens":{{"input":1,"output":1,"cached":0}},"model":"gemini-2.5-flash"}}
{{"id":"u2","timestamp":"2026-05-18T00:00:03Z","type":"user","content":[{{"text":"again"}}]}}
{{"id":"g2","timestamp":"2026-05-18T00:00:04Z","type":"gemini","content":"b","thoughts":[],"tokens":{{"input":1,"output":1,"cached":0}},"model":"gemini-2.5-pro"}}"#
        );
        let t = parse_gemini_transcript_content(&body, agent_id());
        let models: Vec<_> = t
            .turns
            .iter()
            .filter_map(|turn| match turn {
                Turn::Agent { model, effort, .. } => Some((model.clone(), effort.clone())),
                Turn::User { .. } => None,
            })
            .collect();
        assert_eq!(
            models,
            vec![
                (Some("gemini-2.5-flash".to_owned()), None),
                (Some("gemini-2.5-pro".to_owned()), None),
            ]
        );
        assert_eq!(t.meta.as_ref().unwrap().model, "gemini-2.5-flash");
    }

    #[test]
    fn parse_dedupes_gemini_records_by_id_last_wins() {
        // Two gemini records sharing `id` collapse to the last one. The
        // assertion: only the second record's content survives as a Text
        // item.
        let session_id = Uuid::parse_str("00000000-0000-4000-8000-0000000000aa").unwrap();
        let body = format!(
            r#"{{"sessionId":"{session_id}","kind":"main","startTime":"2026-05-18T00:00:00Z"}}
{{"id":"u1","timestamp":"2026-05-18T00:00:01Z","type":"user","content":[{{"text":"hi"}}]}}
{{"id":"g1","timestamp":"2026-05-18T00:00:02Z","type":"gemini","content":"first-draft","thoughts":[],"tokens":{{"input":1,"output":1,"cached":0}},"model":"gemini-3-flash-preview"}}
{{"id":"g1","timestamp":"2026-05-18T00:00:03Z","type":"gemini","content":"final","thoughts":[],"tokens":{{"input":1,"output":1,"cached":0}},"model":"gemini-3-flash-preview"}}"#
        );
        let t = parse_gemini_transcript_content(&body, agent_id());
        assert!(t.warnings.is_empty(), "no warnings: {:?}", t.warnings);
        let Turn::Agent { items, .. } = t
            .turns
            .iter()
            .find(|turn| matches!(turn, Turn::Agent { .. }))
            .expect("one agent turn")
        else {
            unreachable!();
        };
        let texts: Vec<&str> = items
            .iter()
            .filter_map(|item| match item {
                TurnItem::Text {
                    kind: ContentKind::Text,
                    text,
                } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            texts,
            vec!["final"],
            "dedupe-by-id last-wins should drop 'first-draft'"
        );
    }

    #[test]
    fn parse_ignores_thoughts_but_keeps_answer_and_token_telemetry() {
        // Gemini writes reasoning (`thoughts`) only to disk, never to the live
        // stream, so we deliberately do not surface it (stale-on-reopen UX).
        // The answer text and token usage must still come through.
        let session_id = Uuid::parse_str("00000000-0000-4000-8000-0000000000bb").unwrap();
        let body = format!(
            r#"{{"sessionId":"{session_id}","kind":"main","startTime":"2026-05-18T00:00:00Z"}}
{{"id":"u1","timestamp":"2026-05-18T00:00:01Z","type":"user","content":[{{"text":"think out loud"}}]}}
{{"id":"g1","timestamp":"2026-05-18T00:00:02Z","type":"gemini","content":"reply","thoughts":[{{"subject":"plan","description":"do the thing"}}],"tokens":{{"input":1,"output":1,"cached":0}},"model":"gemini-3-flash-preview"}}"#
        );
        let t = parse_gemini_transcript_content(&body, agent_id());
        let Turn::Agent { items, usage, .. } = t
            .turns
            .iter()
            .find(|turn| matches!(turn, Turn::Agent { .. }))
            .expect("one agent turn")
        else {
            unreachable!();
        };
        // The thought is not surfaced as a Thinking item...
        assert!(
            !items.iter().any(|item| matches!(
                item,
                TurnItem::Text {
                    kind: ContentKind::Thinking,
                    ..
                }
            )),
            "gemini thoughts must not surface as Thinking items"
        );
        // ...but the answer text still does.
        let texts: Vec<&str> = items
            .iter()
            .filter_map(|item| match item {
                TurnItem::Text {
                    kind: ContentKind::Text,
                    text,
                } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, vec!["reply"]);
        // Token telemetry is independent of thoughts and still flows through.
        let usage = usage.as_ref().expect("usage present");
        assert_eq!(usage.input_tokens, 1);
        assert_eq!(usage.output_tokens, 1);
    }

    #[test]
    fn load_gemini_transcript_returns_loader_meta_when_projects_json_missing() {
        // Even with no session file (agent never dispatched), meta is
        // populated from the MCP + skills loaders so the sidebar
        // populates the moment the agent is selected. Matches the
        // Claude / Codex hydration shape.
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let session_id = Uuid::new_v4();
        let t = load_gemini_transcript(home.path(), cwd.path(), session_id, agent_id()).unwrap();
        assert!(t.turns.is_empty());
        assert!(t.warnings.is_empty());
        // Meta is Some with structurally-valid empty registries — no
        // files are staged in this test so both loaders return [].
        let meta = t
            .meta
            .expect("meta must be present even without a session file");
        assert!(meta.mcp_servers.is_empty());
        assert!(meta.skills.is_empty());
        assert!(meta.model.is_empty());
    }

    #[test]
    fn load_gemini_transcript_returns_default_when_chats_dir_missing() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        stage_projects_json_wrapped(home.path(), cwd.path(), "proj");
        let session_id = Uuid::new_v4();
        let t = load_gemini_transcript(home.path(), cwd.path(), session_id, agent_id()).unwrap();
        assert!(t.turns.is_empty());
        assert!(t.warnings.is_empty());
    }

    #[test]
    fn load_gemini_transcript_reads_happy_path_fixture() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let session_id = Uuid::parse_str("00000000-0000-4000-8000-000000000001").unwrap();
        let prefix = id_prefix(&session_id);
        stage_session_file(
            home.path(),
            cwd.path(),
            "proj",
            &format!("session-2026-05-17T22-11-{prefix}.jsonl"),
            HAPPY_PATH_FIXTURE,
        );

        let t = load_gemini_transcript(home.path(), cwd.path(), session_id, agent_id()).unwrap();
        assert_eq!(t.turns.len(), 2);
        assert!(t.warnings.is_empty());
    }

    #[test]
    fn load_gemini_transcript_merges_content_across_multiple_files_with_same_session() {
        // Real-world Gemini behavior: each `--resume` invocation creates
        // a new session file with just a header; the actual conversation
        // history lives in the first file (or any subset). All files
        // share the same sessionId. The hydrator must merge content
        // across all matching files so the user's conversation is
        // visible regardless of which file `read_dir` happened to return
        // first.
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        stage_projects_json_wrapped(home.path(), cwd.path(), "proj");
        let chats = home
            .path()
            .join(".gemini")
            .join("tmp")
            .join("proj")
            .join("chats");
        std::fs::create_dir_all(&chats).unwrap();

        let session_id = Uuid::parse_str("00000000-0000-4000-8000-0000000000bb").unwrap();
        let prefix = id_prefix(&session_id);
        let header = format!(
            r#"{{"sessionId":"{session_id}","projectHash":"x","startTime":"2026-05-19T05:04:00Z","kind":"main"}}"#
        );

        // File 1: full conversation.
        let full = format!(
            r#"{header}
{{"id":"u1","timestamp":"2026-05-19T05:04:01Z","type":"user","content":[{{"text":"hi"}}]}}
{{"id":"g1","timestamp":"2026-05-19T05:04:02Z","type":"gemini","content":"hello","thoughts":[],"tokens":{{"input":10,"output":1,"cached":0}},"model":"gemini-3-flash-preview"}}
"#
        );
        std::fs::write(
            chats.join(format!("session-2026-05-19T05-04-{prefix}.jsonl")),
            full,
        )
        .unwrap();
        // Files 2 + 3: header-only stubs from later resume invocations.
        std::fs::write(
            chats.join(format!("session-2026-05-19T05-10-{prefix}.jsonl")),
            format!("{header}\n"),
        )
        .unwrap();
        std::fs::write(
            chats.join(format!("session-2026-05-19T05-47-{prefix}.jsonl")),
            format!("{header}\n"),
        )
        .unwrap();

        let t = load_gemini_transcript(home.path(), cwd.path(), session_id, agent_id()).unwrap();
        assert_eq!(
            t.turns.len(),
            2,
            "merged hydration must surface the full conversation regardless of \
             file-iteration order; got: {:?}",
            t.turns
        );
        let meta = t.meta.expect("meta must be populated");
        assert_eq!(
            meta.model, "gemini-3-flash-preview",
            "parser model from the content-bearing file must survive the merge"
        );
    }

    #[test]
    fn load_gemini_transcript_unambiguous_path_merges_loader_meta_with_parser_model() {
        // Pins the contract that the loaded-session path (the
        // `CandidateMatch::Unambiguous` branch) merges
        // parser-extracted `model` with loader-loaded MCP / skills via
        // `merge_meta_with_loaders`. Stages a real session file, a
        // user-scope MCP server, and a workspace-scope skill — the
        // hydrated meta must carry all three. A future regression that
        // drops the merge in the Unambiguous branch fails here.
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();

        // Stage MCP + skills.
        let user_gemini = home.path().join(".gemini");
        std::fs::create_dir_all(&user_gemini).unwrap();
        std::fs::write(
            user_gemini.join("settings.json"),
            r#"{"mcpServers":{"loader-mcp":{"command":"x"}}}"#,
        )
        .unwrap();
        let workspace_skill_dir = cwd
            .path()
            .join(".gemini")
            .join("skills")
            .join("loader-skill");
        std::fs::create_dir_all(&workspace_skill_dir).unwrap();
        std::fs::write(workspace_skill_dir.join("SKILL.md"), "# skill").unwrap();

        // Stage a real session file.
        let session_id = Uuid::parse_str("00000000-0000-4000-8000-000000000001").unwrap();
        let prefix = id_prefix(&session_id);
        stage_session_file(
            home.path(),
            cwd.path(),
            "proj",
            &format!("session-2026-05-17T22-11-{prefix}.jsonl"),
            HAPPY_PATH_FIXTURE,
        );

        let t = load_gemini_transcript(home.path(), cwd.path(), session_id, agent_id()).unwrap();
        assert_eq!(t.turns.len(), 2, "session file must have parsed");
        assert!(t.warnings.is_empty());

        let meta = t.meta.expect("meta must merge parser + loader output");
        assert_eq!(
            meta.model, "gemini-3-flash-preview",
            "parser-extracted model must survive the loader merge"
        );
        let mcp_names: Vec<&str> = meta.mcp_servers.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            mcp_names,
            vec!["loader-mcp"],
            "loader-loaded MCP server must be present"
        );
        assert_eq!(
            meta.skills,
            vec!["loader-skill".to_owned()],
            "loader-loaded skill must be present"
        );
    }

    #[test]
    fn load_gemini_transcript_path_layer_demixes_case1_collision() {
        // Two separate files with identical 8-char prefix in their
        // filename suffix (different timestamps in the filename). Each
        // file holds a single conversation. `select_candidate_by_header`
        // picks the right one by inspecting first-record sessionId.
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        stage_projects_json_wrapped(home.path(), cwd.path(), "proj");
        let chats = home
            .path()
            .join(".gemini")
            .join("tmp")
            .join("proj")
            .join("chats");
        std::fs::create_dir_all(&chats).unwrap();

        let id_a = Uuid::parse_str("00000000-0000-4000-8000-000000000010").unwrap();
        let id_b = Uuid::parse_str("00000000-0000-4000-8000-000000000020").unwrap();
        // Identical id8 prefixes (both start with "00000000"); different
        // minute-precision timestamps in the filename → two separate
        // files matching the same glob.
        let prefix = id_prefix(&id_a);
        assert_eq!(prefix, id_prefix(&id_b));

        let body_a = format!(
            r#"{{"sessionId":"{id_a}","kind":"main","startTime":"2026-05-18T00:00:00Z"}}
{{"id":"u1","timestamp":"2026-05-18T00:00:01Z","type":"user","content":[{{"text":"alpha"}}]}}
{{"id":"g1","timestamp":"2026-05-18T00:00:02Z","type":"gemini","content":"A","thoughts":[],"tokens":{{"input":1,"output":1,"cached":0}},"model":"gemini-3-flash-preview"}}"#
        );
        let body_b = format!(
            r#"{{"sessionId":"{id_b}","kind":"main","startTime":"2026-05-18T00:05:00Z"}}
{{"id":"u2","timestamp":"2026-05-18T00:05:01Z","type":"user","content":[{{"text":"beta"}}]}}
{{"id":"g2","timestamp":"2026-05-18T00:05:02Z","type":"gemini","content":"B","thoughts":[],"tokens":{{"input":1,"output":1,"cached":0}},"model":"gemini-3-flash-preview"}}"#
        );
        std::fs::write(
            chats.join(format!("session-2026-05-18T00-00-{prefix}.jsonl")),
            body_a,
        )
        .unwrap();
        std::fs::write(
            chats.join(format!("session-2026-05-18T00-05-{prefix}.jsonl")),
            body_b,
        )
        .unwrap();

        // Each target loads only its own content; neither sees the other.
        let t_a = load_gemini_transcript(home.path(), cwd.path(), id_a, agent_id()).unwrap();
        let t_b = load_gemini_transcript(home.path(), cwd.path(), id_b, agent_id()).unwrap();
        for (t, expected_prompt) in [(&t_a, "alpha"), (&t_b, "beta")] {
            let Turn::User { text, .. } = t
                .turns
                .iter()
                .find(|turn| matches!(turn, Turn::User { .. }))
                .expect("user turn present")
            else {
                unreachable!();
            };
            assert_eq!(text, expected_prompt);
        }
    }

    #[test]
    fn load_gemini_transcript_returns_default_when_no_candidate_header_matches_target() {
        // One file present matching the prefix glob, but its first header
        // sessionId is for a different conversation. The path layer
        // refuses to read it.
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let stored = Uuid::parse_str("00000000-0000-4000-8000-000000000010").unwrap();
        let prefix = id_prefix(&stored);
        let body = format!(
            r#"{{"sessionId":"{stored}","kind":"main","startTime":"2026-05-18T00:00:00Z"}}
{{"id":"u1","timestamp":"2026-05-18T00:00:01Z","type":"user","content":[{{"text":"x"}}]}}"#
        );
        stage_session_file(
            home.path(),
            cwd.path(),
            "proj",
            &format!("session-2026-05-18T00-00-{prefix}.jsonl"),
            &body,
        );

        let asked_for = Uuid::parse_str("00000000-0000-4000-8000-000000000099").unwrap();
        assert_eq!(prefix, id_prefix(&asked_for));
        let t = load_gemini_transcript(home.path(), cwd.path(), asked_for, agent_id()).unwrap();
        assert!(t.turns.is_empty());
        assert!(t.warnings.is_empty());
        // Even when no candidate matches the target, meta is populated
        // from the MCP / skills loaders so the sidebar surfaces
        // structurally consistent state — same outcome shape as the
        // never-dispatched case. A future regression that drops the
        // loader call in this branch fails here.
        let meta = t
            .meta
            .expect("loader meta must be populated even when no candidate matches");
        assert!(meta.mcp_servers.is_empty());
        assert!(meta.skills.is_empty());
    }

    #[test]
    fn load_gemini_transcript_returns_collision_warning_on_multi_header_file_for_either_target() {
        // The captured worst-case fixture: one file, two distinct
        // sessionId headers, events interleaved. Both targets must
        // surface the ambiguity warning — silently empty for one and
        // warning for the other would mean one of two collided agents
        // looks "never dispatched" instead of "blocked on ambiguity."
        let target_a = Uuid::parse_str("00000000-0000-4000-8000-000000000009").unwrap();
        let target_b = Uuid::parse_str("00000000-0000-4000-8000-00000000000A").unwrap();
        for target in [target_a, target_b] {
            let home = TempDir::new().unwrap();
            let cwd = TempDir::new().unwrap();
            let prefix = id_prefix(&target);
            stage_session_file(
                home.path(),
                cwd.path(),
                "proj",
                &format!("session-2026-05-17T22-20-{prefix}.jsonl"),
                INTERLEAVED_FIXTURE,
            );

            let t = load_gemini_transcript(home.path(), cwd.path(), target, agent_id()).unwrap();
            assert!(
                t.turns.is_empty(),
                "ambiguous file must hydrate no turns (target {target}); got {:?}",
                t.turns
            );
            assert_eq!(
                t.warnings.len(),
                1,
                "exactly one warning expected for target {target}"
            );
            assert!(
                t.warnings[0].reason.contains("multiple sessions"),
                "warning must surface the ambiguity for target {target}: {:?}",
                t.warnings[0]
            );
        }
    }
}
