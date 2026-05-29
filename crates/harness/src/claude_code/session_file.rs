//! Claude Code session-file parser for transcript hydration.
//!
//! Claude Code writes one JSONL file per session at
//! `<home>/.claude/projects/<encoded-cwd>/<session_id>.jsonl`. The on-disk
//! vocabulary differs from the live stream — records have a top-level
//! `type` discriminator with values like `user`, `assistant`, `attachment`,
//! `last-prompt`, `queue-operation`, `system`, etc. No `system/init` or
//! `result` record on disk; those are stream-only. Cannot reuse the stream
//! parser as-is.
//!
//! ## Record mapping
//!
//! - `user` with string `message.content` → a fresh user prompt; closes
//!   any pending agent turn and opens a new `Turn::User`.
//! - `user` with array `message.content` containing `tool_result` blocks →
//!   tool-result records that pair with the current agent turn's open
//!   tools by `tool_use_id`; does **not** start a new user turn.
//! - `assistant` → accumulates into the current pending agent turn. Each
//!   content block in `message.content`:
//!   - `type: "text"` → append `TurnItem::Text`.
//!   - `type: "tool_use"` → append `TurnItem::Tool` (`output`/`completed_at`
//!     filled in by the later paired user/`tool_result` record).
//!   - other types (e.g., `thinking`) → currently dropped with a warning;
//!     reserved for future expansion.
//!
//! ## Lifecycle
//!
//! Agent turns open on the first `assistant` record after a `user` prompt
//! and close on the next `user` prompt or EOF. No on-disk terminal marker
//! exists for "this turn completed" — the only signal is the appearance
//! of the next prompt. EOF without a next prompt is treated as `Complete`
//! (the typical "session ended cleanly" case); genuine truncation is
//! indistinguishable from a fresh-completed final turn. The implementation
//! is asymmetric with Codex's parser, which defaults to `Failed` on EOF
//! because Codex *does* emit a per-turn terminal marker — see
//! `crates/harness/src/codex/session_file.rs::finalize`.
//!
//! ## Path resolution
//!
//! Primary: `<home>/.claude/projects/<encoded-cwd>/<session_id>.jsonl` via
//! [`crate::claude_session_file_path`]. If the primary path is missing,
//! a secondary fallback scans `<home>/.claude/projects/*/<session_id>.jsonl`
//! — session IDs are UUID v7 and globally unique, so any match is the
//! file we want. The fallback exists for resilience against cwd encoding
//! drift across Claude CLI versions.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde_json::Value;
use switchboard_core::AgentId;
use uuid::Uuid;

use crate::events::{ContentKind, TurnId, TurnUsage};
use crate::parser::classify_claude_tool_kind;
use crate::transcript::{
    LoadTranscriptError, LoadedTranscript, ParseWarning, SessionMetaInfo, Turn, TurnItem,
    TurnStatus, merge_meta_with_loaders,
};

use super::claude_session_file_path;
use super::config::load_mcp_servers;
use super::skills::load_skills;

/// Load a Claude Code session file for `session_id` and project a
/// `LoadedTranscript`.
///
/// Returns `Ok(LoadedTranscript { turns: vec![], warnings: vec![] })` when
/// no session file can be found at the primary path or via the fallback
/// scan — that is the "fresh agent" outcome shape. `Err` is reserved for
/// I/O failures on a file that exists.
///
/// `home_dir` is injected for testability (does not consult `$HOME`).
pub fn load_claude_transcript(
    home_dir: &Path,
    cwd: &Path,
    session_id: Uuid,
    agent_id: AgentId,
) -> Result<LoadedTranscript, LoadTranscriptError> {
    let Some(path) = resolve_session_path(home_dir, cwd, session_id) else {
        return Ok(LoadedTranscript {
            meta: Some(merge_meta_with_loaders(
                None,
                load_mcp_servers(home_dir, cwd),
                load_skills(home_dir, cwd),
            )),
            ..LoadedTranscript::default()
        });
    };

    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(LoadedTranscript::default());
        }
        Err(e) => return Err(LoadTranscriptError::Io(e)),
    };

    let mut state = ReconstructionState::new(agent_id);
    for (idx, line) in content.lines().enumerate() {
        let line_number = idx + 1;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(line) {
            Ok(value) => state.ingest_record(line_number, &value),
            Err(e) => state.warn(line_number, format!("malformed JSON: {e}")),
        }
    }
    let mut transcript = state.finalize();
    transcript.meta = Some(merge_meta_with_loaders(
        transcript.meta.take(),
        load_mcp_servers(home_dir, cwd),
        load_skills(home_dir, cwd),
    ));
    Ok(transcript)
}

fn resolve_session_path(home_dir: &Path, cwd: &Path, session_id: Uuid) -> Option<PathBuf> {
    let canonical = cwd.canonicalize().ok()?;
    let primary = claude_session_file_path(home_dir, &canonical, &session_id);
    if primary.exists() {
        return Some(primary);
    }
    fallback_scan(home_dir, session_id)
}

/// Scan `<home>/.claude/projects/*/<session_id>.jsonl`. Returns the first
/// match (session IDs are globally unique so any match is correct).
fn fallback_scan(home_dir: &Path, session_id: Uuid) -> Option<PathBuf> {
    let projects_dir = home_dir.join(".claude").join("projects");
    let filename = format!("{session_id}.jsonl");
    let entries = std::fs::read_dir(&projects_dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path().join(&filename);
        if path.is_file() {
            return Some(path);
        }
    }
    None
}

/// In-progress reconstruction state. Walks records in order, opening a
/// fresh `Turn::User` on each prompt and accumulating `assistant` records
/// into the corresponding `Turn::Agent`.
struct ReconstructionState {
    agent_id: AgentId,
    turns: Vec<Turn>,
    current_agent: Option<AgentTurnBuilder>,
    first_model: Option<String>,
    warnings: Vec<ParseWarning>,
    /// `tool_result` records whose matching `tool_use` hasn't been seen
    /// yet. Claude Code 2.1.150 was observed to write `tool_result`
    /// records to disk *before* their matching `tool_use` (within the
    /// same turn; ~1s gap; observed in session
    /// `22300f1b-3efe-4dbc-a4a0-7c1c954d1da2.jsonl` lines 1406/1408 and
    /// 1607/1609). `handle_assistant` drains matches when the
    /// corresponding `tool_use` arrives.
    ///
    /// Effectively per-turn lifetime: flushed as warnings on every new
    /// user prompt (`handle_user` string branch) and on `finalize`.
    /// A deferred entry never crosses a turn boundary — even if a same
    /// `tool_use_id` appears in a later turn, the stale entry has been
    /// flushed by then and won't silently bind to the new turn's tool.
    pending_tool_results: Vec<DeferredToolResult>,
}

struct AgentTurnBuilder {
    turn_id: TurnId,
    agent_id: AgentId,
    started_at: DateTime<Utc>,
    last_seen_at: DateTime<Utc>,
    items: Vec<TurnItem>,
    usage: Option<TurnUsage>,
}

struct DeferredToolResult {
    tool_use_id: String,
    output: String,
    is_error: bool,
    completed_at: Option<DateTime<Utc>>,
    line_number: usize,
}

impl ReconstructionState {
    fn new(agent_id: AgentId) -> Self {
        Self {
            agent_id,
            turns: Vec::new(),
            current_agent: None,
            first_model: None,
            warnings: Vec::new(),
            pending_tool_results: Vec::new(),
        }
    }

    fn warn(&mut self, line_number: usize, reason: impl Into<String>) {
        self.warnings.push(ParseWarning {
            line_number,
            reason: reason.into(),
        });
    }

    fn ingest_record(&mut self, line_number: usize, record: &Value) {
        let Some(record_type) = record.get("type").and_then(Value::as_str) else {
            return;
        };
        match record_type {
            "user" => self.handle_user(line_number, record),
            "assistant" => self.handle_assistant(line_number, record),
            _ => {
                // Other record types (`attachment`, `queue-operation`,
                // `system`, `agent-name`, `ai-title`, `last-prompt`,
                // `file-history-snapshot`, `permission-mode`) carry session
                // metadata that doesn't affect the user/agent transcript
                // shape. Silently skipped — not a warning condition.
            }
        }
    }

    /// Drain `pending_tool_results` into warnings. Called at each turn
    /// boundary (a new user prompt) and at `finalize` — deferred
    /// `tool_result` entries must not survive across the turn they
    /// belong to, or a same-id `tool_use` in a later turn would silently
    /// bind the stale output and corrupt the reconstructed transcript.
    fn flush_deferred_as_warnings(&mut self) {
        for deferred in std::mem::take(&mut self.pending_tool_results) {
            self.warnings.push(ParseWarning {
                line_number: deferred.line_number,
                reason: format!(
                    "tool_result for {} never matched a tool_use",
                    deferred.tool_use_id
                ),
            });
        }
    }

    fn handle_user(&mut self, line_number: usize, record: &Value) {
        let message = record.get("message");
        let content = message.and_then(|m| m.get("content"));
        match content {
            Some(Value::String(text)) => {
                // Fresh user prompt — close any open agent turn, flush any
                // unmatched deferred tool_results (they belonged to the
                // turn that just ended; they must not carry into the new
                // turn), then open a new User turn.
                self.close_current_agent(TurnStatus::Complete);
                self.flush_deferred_as_warnings();
                let started_at = parse_timestamp(record).unwrap_or_else(Utc::now);
                self.turns.push(Turn::User {
                    turn_id: Uuid::now_v7(),
                    agent_id: self.agent_id,
                    started_at,
                    text: text.clone(),
                });
            }
            Some(Value::Array(blocks)) => {
                // Array content from a `user` record carries tool_result
                // blocks that pair with the current agent turn's open tools.
                let completed_at = parse_timestamp(record);
                for block in blocks {
                    if block.get("type").and_then(Value::as_str) == Some("tool_result") {
                        self.handle_tool_result(line_number, block, completed_at);
                    }
                }
            }
            _ => {}
        }
    }

    fn handle_assistant(&mut self, line_number: usize, record: &Value) {
        if self.first_model.is_none()
            && let Some(model) = record
                .get("message")
                .and_then(|m| m.get("model"))
                .and_then(Value::as_str)
        {
            self.first_model = Some(model.to_owned());
        }

        let timestamp = parse_timestamp(record).unwrap_or_else(Utc::now);
        let builder = self.current_agent.get_or_insert_with(|| AgentTurnBuilder {
            turn_id: Uuid::now_v7(),
            agent_id: self.agent_id,
            started_at: timestamp,
            last_seen_at: timestamp,
            items: Vec::new(),
            usage: None,
        });
        builder.last_seen_at = timestamp;

        if let Some(usage) = record
            .get("message")
            .and_then(|m| m.get("usage"))
            .and_then(Value::as_object)
        {
            builder.usage = Some(parse_usage(usage));
        }

        let Some(blocks) = record
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(Value::as_array)
        else {
            return;
        };

        for block in blocks {
            let block_type = block.get("type").and_then(Value::as_str);
            match block_type {
                Some("text") => {
                    if let Some(text) = block.get("text").and_then(Value::as_str) {
                        builder.items.push(TurnItem::Text {
                            kind: ContentKind::Text,
                            text: text.to_owned(),
                        });
                    }
                }
                Some("tool_use") => {
                    let Some(id) = block.get("id").and_then(Value::as_str) else {
                        self.warnings.push(ParseWarning {
                            line_number,
                            reason: "tool_use block missing id; dropped".to_owned(),
                        });
                        continue;
                    };
                    let name = block.get("name").and_then(Value::as_str).unwrap_or("");
                    let input = block.get("input").cloned().unwrap_or(Value::Null);

                    // Late-bind: if a tool_result for this id arrived
                    // earlier (Claude can write tool_result before tool_use
                    // — see ReconstructionState::pending_tool_results),
                    // pull it from the deferred queue and apply it now.
                    let deferred = self
                        .pending_tool_results
                        .iter()
                        .position(|d| d.tool_use_id == id)
                        .map(|pos| self.pending_tool_results.swap_remove(pos));
                    let (output, is_error, completed_at) = match deferred {
                        Some(d) => (Some(d.output), Some(d.is_error), d.completed_at),
                        None => (None, None, None),
                    };

                    builder.items.push(TurnItem::Tool {
                        tool_use_id: id.to_owned(),
                        kind: classify_claude_tool_kind(name),
                        name: name.to_owned(),
                        input,
                        output,
                        is_error,
                        started_at: timestamp,
                        completed_at,
                    });
                }
                _ => {
                    // `thinking` and any other future block type — silently
                    // skipped for now. Future reasoning UI work will revisit.
                }
            }
        }
    }

    fn handle_tool_result(
        &mut self,
        line_number: usize,
        block: &Value,
        completed_at: Option<DateTime<Utc>>,
    ) {
        let Some(tool_use_id) = block.get("tool_use_id").and_then(Value::as_str) else {
            return;
        };
        let is_error = block
            .get("is_error")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let output = extract_tool_result_text(block.get("content"));

        // Happy path: matching tool_use already in the current builder.
        if let Some(builder) = self.current_agent.as_mut() {
            for item in &mut builder.items {
                if let TurnItem::Tool {
                    tool_use_id: id,
                    output: out,
                    is_error: err,
                    completed_at: cat,
                    ..
                } = item
                    && id == tool_use_id
                {
                    *out = Some(output);
                    *err = Some(is_error);
                    *cat = completed_at;
                    return;
                }
            }
        }

        // No matching tool_use yet — either no current agent turn at all
        // (the tool_result preceded any assistant record) or the builder
        // exists but hasn't seen this id (the matching tool_use is later
        // in the file). Defer; `handle_assistant` will bind it when the
        // tool_use arrives, or `finalize` will warn if it never does.
        self.pending_tool_results.push(DeferredToolResult {
            tool_use_id: tool_use_id.to_owned(),
            output,
            is_error,
            completed_at,
            line_number,
        });
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
        // Any in-progress agent turn at EOF defaults to `Complete`. Claude's
        // session-file format has no per-turn terminal marker — the only
        // signal a turn finished is the next user prompt arriving. That
        // makes "truncated mid-turn" and "completed-but-no-next-prompt-yet"
        // indistinguishable from disk state alone. Conservatively defaulting
        // to Complete matches the typical "session ended cleanly" case.
        //
        // **Asymmetric with Codex on purpose**: Codex emits
        // `event_msg/task_complete` per turn, so its `finalize` defaults to
        // `Failed` when the marker is missing. See `crates/harness/src/
        // codex/session_file.rs::CodexReconstruction::finalize` for the
        // other side of the asymmetry.
        //
        // Ordering: close the current agent *before* flushing deferred
        // tool_results. Binding happens in `handle_assistant`, not in
        // `close_current_agent`, so by this point any remaining deferreds
        // are genuinely unmatched (the matching `tool_use` never appeared
        // in the final turn's records).
        self.close_current_agent(TurnStatus::Complete);
        self.flush_deferred_as_warnings();
        let meta = self.first_model.map(|model| SessionMetaInfo {
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

/// Project Claude's `assistant.message.usage` payload into `TurnUsage`.
/// `context_window` is unrecoverable from disk (lives in stream-only
/// `result.modelUsage`) — emit `None`. `total_cost_usd` is similarly
/// stream-only; emit `None`.
fn parse_usage(usage: &serde_json::Map<String, Value>) -> TurnUsage {
    TurnUsage {
        input_tokens: usage
            .get("input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        output_tokens: usage
            .get("output_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        cached_input_tokens: usage.get("cache_read_input_tokens").and_then(Value::as_u64),
        reasoning_output_tokens: usage.get("reasoning_output_tokens").and_then(Value::as_u64),
        context_window: None,
        total_cost_usd: None,
    }
}

/// Extract the displayable text from a `tool_result.content` value. Claude
/// emits `content` as either a plain string or an array of `{type:"text",
/// text: "..."}` blocks; join the text values in order.
fn extract_tool_result_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|b| {
                if b.get("type").and_then(Value::as_str) == Some("text") {
                    b.get("text").and_then(Value::as_str).map(str::to_owned)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::ToolKind;
    use serde_json::json;
    use tempfile::TempDir;

    fn stage_session_file(home: &Path, cwd: &Path, session_id: Uuid, content: &str) -> PathBuf {
        let canonical = cwd.canonicalize().unwrap();
        let path = claude_session_file_path(home, &canonical, &session_id);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, content).unwrap();
        path
    }

    fn user_record(text: &str, timestamp: &str) -> Value {
        json!({
            "type": "user",
            "message": { "role": "user", "content": text },
            "timestamp": timestamp,
        })
    }

    fn assistant_text_record(text: &str, model: &str, timestamp: &str) -> Value {
        json!({
            "type": "assistant",
            "message": {
                "model": model,
                "role": "assistant",
                "content": [{ "type": "text", "text": text }],
                "usage": {
                    "input_tokens": 10,
                    "output_tokens": 5,
                    "cache_read_input_tokens": 2,
                }
            },
            "timestamp": timestamp,
        })
    }

    fn jsonl(records: &[Value]) -> String {
        records
            .iter()
            .map(|r| serde_json::to_string(r).unwrap())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn missing_session_file_returns_empty_transcript() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let result =
            load_claude_transcript(home.path(), cwd.path(), Uuid::now_v7(), Uuid::now_v7())
                .unwrap();
        assert!(result.turns.is_empty());
    }

    #[test]
    fn text_only_turn_produces_user_then_agent() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let session_id = Uuid::now_v7();
        let agent_id = Uuid::now_v7();
        let content = jsonl(&[
            user_record("Say 1", "2026-05-14T04:43:15Z"),
            assistant_text_record("1", "claude-sonnet-4-6", "2026-05-14T04:43:16Z"),
        ]);
        stage_session_file(home.path(), cwd.path(), session_id, &content);

        let result = load_claude_transcript(home.path(), cwd.path(), session_id, agent_id).unwrap();

        assert_eq!(result.turns.len(), 2);
        match &result.turns[0] {
            Turn::User {
                text, agent_id: a, ..
            } => {
                assert_eq!(text, "Say 1");
                assert_eq!(*a, agent_id);
            }
            _ => panic!("expected User turn first"),
        }
        match &result.turns[1] {
            Turn::Agent {
                items,
                status,
                usage,
                agent_id: a,
                ..
            } => {
                assert_eq!(*a, agent_id);
                assert_eq!(items.len(), 1);
                assert!(matches!(&items[0], TurnItem::Text { text, .. } if text == "1"));
                assert!(matches!(status, TurnStatus::Complete));
                let usage = usage.as_ref().unwrap();
                assert_eq!(usage.input_tokens, 10);
                assert_eq!(usage.output_tokens, 5);
                assert!(usage.context_window.is_none());
            }
            _ => panic!("expected Agent turn second"),
        }
        let meta = result.meta.unwrap();
        assert_eq!(meta.model, "claude-sonnet-4-6");
    }

    #[test]
    fn multi_turn_produces_alternating_pairs() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let session_id = Uuid::now_v7();
        let agent_id = Uuid::now_v7();
        let content = jsonl(&[
            user_record("Say 1", "2026-05-14T04:43:15Z"),
            assistant_text_record("1", "claude-sonnet-4-6", "2026-05-14T04:43:16Z"),
            user_record("Say 2", "2026-05-14T04:43:22Z"),
            assistant_text_record("2", "claude-sonnet-4-6", "2026-05-14T04:43:23Z"),
        ]);
        stage_session_file(home.path(), cwd.path(), session_id, &content);

        let result = load_claude_transcript(home.path(), cwd.path(), session_id, agent_id).unwrap();
        assert_eq!(result.turns.len(), 4);
        assert!(matches!(result.turns[0], Turn::User { .. }));
        assert!(matches!(result.turns[1], Turn::Agent { .. }));
        assert!(matches!(result.turns[2], Turn::User { .. }));
        assert!(matches!(result.turns[3], Turn::Agent { .. }));
    }

    #[test]
    fn tool_use_pairs_with_tool_result() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let session_id = Uuid::now_v7();
        let agent_id = Uuid::now_v7();
        let assistant_with_tool = json!({
            "type": "assistant",
            "message": {
                "model": "claude-sonnet-4-6",
                "role": "assistant",
                "content": [
                    { "type": "text", "text": "running bash" },
                    {
                        "type": "tool_use",
                        "id": "toolu_1",
                        "name": "Bash",
                        "input": { "command": "ls" }
                    }
                ],
                "usage": { "input_tokens": 1, "output_tokens": 2 }
            },
            "timestamp": "2026-05-14T04:43:16Z"
        });
        let user_tool_result = json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": "toolu_1",
                    "content": "ok",
                    "is_error": false
                }]
            },
            "timestamp": "2026-05-14T04:43:17Z"
        });
        let content = jsonl(&[
            user_record("Run ls", "2026-05-14T04:43:15Z"),
            assistant_with_tool,
            user_tool_result,
        ]);
        stage_session_file(home.path(), cwd.path(), session_id, &content);

        let result = load_claude_transcript(home.path(), cwd.path(), session_id, agent_id).unwrap();
        assert_eq!(result.turns.len(), 2);
        let Turn::Agent { items, .. } = &result.turns[1] else {
            panic!("expected Agent turn");
        };
        assert_eq!(items.len(), 2);
        assert!(matches!(&items[0], TurnItem::Text { text, .. } if text == "running bash"));
        match &items[1] {
            TurnItem::Tool {
                tool_use_id,
                kind,
                name,
                output,
                is_error,
                completed_at,
                ..
            } => {
                assert_eq!(tool_use_id, "toolu_1");
                assert_eq!(*kind, ToolKind::Builtin);
                assert_eq!(name, "Bash");
                assert_eq!(output.as_deref(), Some("ok"));
                assert_eq!(*is_error, Some(false));
                assert!(completed_at.is_some());
            }
            _ => panic!("expected Tool item"),
        }
    }

    // --- Out-of-order tool_use / tool_result binding (M2 of the parser
    // fidelity plan: Claude 2.1.150 was observed to write tool_result
    // records to disk before their matching tool_use, ~1s gap, in session
    // `22300f1b-3efe-4dbc-a4a0-7c1c954d1da2.jsonl` lines 1406/1408 and
    // 1607/1609). Tests assert on the returned LoadedTranscript — the path
    // production goes through — not on intermediate state. ---

    fn user_tool_result(tool_use_id: &str, output: &str, timestamp: &str) -> Value {
        json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": tool_use_id,
                    "content": output,
                    "is_error": false
                }]
            },
            "timestamp": timestamp
        })
    }

    fn assistant_tool_use(tool_use_id: &str, name: &str, input: &Value, timestamp: &str) -> Value {
        json!({
            "type": "assistant",
            "message": {
                "model": "claude-sonnet-4-6",
                "role": "assistant",
                "content": [{
                    "type": "tool_use",
                    "id": tool_use_id,
                    "name": name,
                    "input": input
                }],
                "usage": { "input_tokens": 1, "output_tokens": 1 }
            },
            "timestamp": timestamp
        })
    }

    /// The bug: Claude 2.1.150 can write a `tool_result` before its matching
    /// `tool_use` in the session file. Pre-M2, the parser dropped the
    /// `tool_result`'s output silently and emitted a "did not match any
    /// open tool" warning. After M2, the deferred queue binds at
    /// `tool_use` time and no warning surfaces.
    #[test]
    fn out_of_order_tool_result_before_tool_use_binds_via_deferred_queue() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let session_id = Uuid::now_v7();
        let agent_id = Uuid::now_v7();
        // Order on disk: user prompt → first assistant record (text) →
        // user/tool_result for toolu_x (before the tool_use exists) →
        // assistant/tool_use toolu_x (binds the deferred result).
        let content = jsonl(&[
            user_record("Run ls", "2026-05-14T04:43:15Z"),
            assistant_text_record("running bash", "claude-sonnet-4-6", "2026-05-14T04:43:16Z"),
            user_tool_result("toolu_x", "ls-output", "2026-05-14T04:43:17Z"),
            assistant_tool_use(
                "toolu_x",
                "Bash",
                &json!({ "command": "ls" }),
                "2026-05-14T04:43:18Z",
            ),
        ]);
        stage_session_file(home.path(), cwd.path(), session_id, &content);

        let result = load_claude_transcript(home.path(), cwd.path(), session_id, agent_id).unwrap();
        assert!(
            result.warnings.is_empty(),
            "out-of-order binding must not warn; got {:?}",
            result.warnings,
        );
        let Turn::Agent { items, .. } = &result.turns[1] else {
            panic!("expected Agent turn");
        };
        let tool = items
            .iter()
            .find(|i| matches!(i, TurnItem::Tool { tool_use_id, .. } if tool_use_id == "toolu_x"))
            .expect("Bash TurnItem must exist");
        match tool {
            TurnItem::Tool {
                output,
                is_error,
                completed_at,
                ..
            } => {
                assert_eq!(output.as_deref(), Some("ls-output"));
                assert_eq!(*is_error, Some(false));
                assert!(completed_at.is_some());
            }
            _ => unreachable!(),
        }
    }

    /// Case-(a) regression guard: `tool_result` arrives before ANY
    /// assistant record for the turn (no `current_agent` yet). The
    /// builder-scoped queue that this milestone replaced would have
    /// missed this case — the warning path "no open agent turn" would
    /// have fired and the output dropped. With the queue on
    /// `ReconstructionState`, the deferred entry survives until
    /// `handle_assistant` creates the builder and binds the match.
    #[test]
    fn tool_result_before_any_assistant_record_binds_when_assistant_arrives() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let session_id = Uuid::now_v7();
        let agent_id = Uuid::now_v7();
        let content = jsonl(&[
            user_record("Run ls", "2026-05-14T04:43:15Z"),
            // tool_result arrives before any assistant record — no
            // current_agent at this point.
            user_tool_result("toolu_x", "ls-output", "2026-05-14T04:43:16Z"),
            // First assistant record creates the builder; tool_use binds
            // the deferred result.
            assistant_tool_use(
                "toolu_x",
                "Bash",
                &json!({ "command": "ls" }),
                "2026-05-14T04:43:17Z",
            ),
        ]);
        stage_session_file(home.path(), cwd.path(), session_id, &content);

        let result = load_claude_transcript(home.path(), cwd.path(), session_id, agent_id).unwrap();
        assert!(
            result.warnings.is_empty(),
            "pre-builder deferred binding must not warn; got {:?}",
            result.warnings,
        );
        let Turn::Agent { items, .. } = &result.turns[1] else {
            panic!("expected Agent turn after user turn");
        };
        match items.iter().find_map(|i| match i {
            TurnItem::Tool {
                tool_use_id,
                output,
                ..
            } if tool_use_id == "toolu_x" => Some(output.as_deref()),
            _ => None,
        }) {
            Some(Some("ls-output")) => {}
            other => panic!("expected toolu_x to be bound with 'ls-output', got {other:?}"),
        }
    }

    /// Genuine miss: a `tool_result` whose `tool_use_id` never appears.
    /// Surfaces as exactly one warning on the **returned transcript** —
    /// guards both the unmatched-detection contract and the
    /// finalize-warning-attachment fix (without that attachment, this
    /// warning would be silently dropped).
    #[test]
    fn unmatched_tool_result_surfaces_as_warning_on_returned_transcript() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let session_id = Uuid::now_v7();
        let agent_id = Uuid::now_v7();
        let content = jsonl(&[
            user_record("Run ls", "2026-05-14T04:43:15Z"),
            assistant_text_record("answering", "claude-sonnet-4-6", "2026-05-14T04:43:16Z"),
            user_tool_result("toolu_ORPHAN", "stray", "2026-05-14T04:43:17Z"),
        ]);
        stage_session_file(home.path(), cwd.path(), session_id, &content);

        let result = load_claude_transcript(home.path(), cwd.path(), session_id, agent_id).unwrap();
        assert_eq!(
            result.warnings.len(),
            1,
            "expected one warning for the unmatched tool_result; got {:?}",
            result.warnings,
        );
        assert!(
            result.warnings[0].reason.contains("toolu_ORPHAN"),
            "warning must name the orphan id; got {:?}",
            result.warnings[0].reason,
        );
        assert!(
            result.warnings[0]
                .reason
                .contains("never matched a tool_use"),
            "warning must use the unified post-M2 wording; got {:?}",
            result.warnings[0].reason,
        );
        // No phantom TurnItem::Tool is created — the orphan result
        // doesn't surface as an empty-output Tool item.
        let Turn::Agent { items, .. } = &result.turns[1] else {
            panic!("expected Agent turn");
        };
        assert!(
            !items.iter().any(
                |i| matches!(i, TurnItem::Tool { tool_use_id, .. } if tool_use_id == "toolu_ORPHAN")
            ),
            "orphan tool_result must not produce a phantom Tool item",
        );
    }

    /// Cross-turn binding guard: an orphan `tool_result` in turn A must
    /// not silently bind to a same-id `tool_use` in turn B. Pre-fix the
    /// deferred queue's per-session lifetime would have carried the
    /// orphan across the turn boundary and attached the stale output to
    /// turn B's tool — exactly the silent corruption the warning surface
    /// is meant to catch. Post-fix the queue flushes as a warning at
    /// every new user prompt.
    #[test]
    fn orphan_tool_result_does_not_bind_across_turn_boundary() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let session_id = Uuid::now_v7();
        let agent_id = Uuid::now_v7();
        let content = jsonl(&[
            // Turn A: user prompt + assistant text + orphan tool_result
            // (no matching tool_use anywhere in turn A).
            user_record("first prompt", "2026-05-14T04:43:15Z"),
            assistant_text_record("answering", "claude-sonnet-4-6", "2026-05-14T04:43:16Z"),
            user_tool_result("toolu_x", "stale-output", "2026-05-14T04:43:17Z"),
            // Turn B: user prompt re-using the same tool_use_id. The
            // orphan from turn A must have been flushed at the boundary,
            // so turn B's tool_use binds with no output.
            user_record("second prompt", "2026-05-14T04:43:18Z"),
            assistant_tool_use(
                "toolu_x",
                "Bash",
                &json!({ "command": "ls" }),
                "2026-05-14T04:43:19Z",
            ),
        ]);
        stage_session_file(home.path(), cwd.path(), session_id, &content);

        let result = load_claude_transcript(home.path(), cwd.path(), session_id, agent_id).unwrap();
        assert_eq!(
            result.warnings.len(),
            1,
            "turn-A orphan must surface as exactly one warning; got {:?}",
            result.warnings,
        );
        assert!(
            result.warnings[0].reason.contains("toolu_x"),
            "warning must name the orphan id; got {:?}",
            result.warnings[0].reason,
        );

        // Turn B's tool_use must NOT have bound the stale output. Find
        // turn B's Agent turn and assert its toolu_x has output: None.
        let turn_b_agent = result
            .turns
            .iter()
            .filter_map(|t| match t {
                Turn::Agent { items, .. } => Some(items),
                _ => None,
            })
            .next_back()
            .expect("expected an Agent turn from turn B");
        let bound = turn_b_agent.iter().find_map(|i| match i {
            TurnItem::Tool {
                tool_use_id,
                output,
                ..
            } if tool_use_id == "toolu_x" => Some(output.as_deref()),
            _ => None,
        });
        assert_eq!(
            bound,
            Some(None),
            "turn B's toolu_x must have output=None; the orphan from turn A must not bind across the boundary",
        );
    }

    /// Disk-side regression guard for the subagent rendering contract.
    ///
    /// Claude's main session file collapses every delegation to (parent's
    /// `Agent` `tool_use` + aggregate `tool_result`); subagent internals
    /// live in `<session-id>/subagents/agent-<id>.jsonl`, which Switchboard
    /// does not read. Verified at scale 2026-05-27: zero non-null
    /// `parent_tool_use_id` values across 317 historical main session files
    /// (see `harness-behavior.md` §6).
    ///
    /// If Claude ever inlines subagent records into the main session file,
    /// this test breaks loudly — at which point the session-file parser
    /// would need its own `parent_tool_use_id` filter to keep the
    /// rehydrated view aligned with the stream parser (which collapses
    /// in-memory; see `parser.rs` `parse_line` short-circuit).
    #[test]
    fn delegation_session_file_renders_as_single_parent_tool_call() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let session_id = Uuid::now_v7();
        let agent_id = Uuid::now_v7();
        let assistant_agent_call = json!({
            "type": "assistant",
            "message": {
                "model": "claude-sonnet-4-6",
                "role": "assistant",
                "content": [{
                    "type": "tool_use",
                    "id": "toolu_PARENT_AGENT_CALL",
                    "name": "Agent",
                    "input": {
                        "description": "<redacted>",
                        "prompt": "<redacted>",
                        "subagent_type": "general-purpose"
                    }
                }],
                "usage": { "input_tokens": 1, "output_tokens": 1 }
            },
            "parent_tool_use_id": null,
            "timestamp": "2026-05-27T21:00:00Z"
        });
        let aggregate_tool_result = json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": "toolu_PARENT_AGENT_CALL",
                    "content": "<redacted: subagent's aggregate report>",
                    "is_error": false
                }]
            },
            "parent_tool_use_id": null,
            "timestamp": "2026-05-27T21:00:05Z"
        });
        let content = jsonl(&[
            user_record("delegate to a subagent", "2026-05-27T20:59:55Z"),
            assistant_agent_call,
            aggregate_tool_result,
        ]);
        stage_session_file(home.path(), cwd.path(), session_id, &content);

        let result = load_claude_transcript(home.path(), cwd.path(), session_id, agent_id).unwrap();
        assert_eq!(result.turns.len(), 2);
        assert!(
            result.warnings.is_empty(),
            "delegation should hydrate cleanly with no warnings; got {:?}",
            result.warnings
        );
        let Turn::Agent { items, .. } = &result.turns[1] else {
            panic!("expected Agent turn");
        };
        assert_eq!(
            items.len(),
            1,
            "expected exactly one tool call (the parent's Agent call); got {items:#?}",
        );
        match &items[0] {
            TurnItem::Tool {
                tool_use_id,
                name,
                output,
                is_error,
                ..
            } => {
                assert_eq!(tool_use_id, "toolu_PARENT_AGENT_CALL");
                assert_eq!(name, "Agent");
                assert_eq!(
                    output.as_deref(),
                    Some("<redacted: subagent's aggregate report>")
                );
                assert_eq!(*is_error, Some(false));
            }
            other => panic!("expected Tool item, got {other:?}"),
        }
    }

    #[test]
    fn mcp_tool_use_classifies_as_mcp_kind() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let session_id = Uuid::now_v7();
        let agent_id = Uuid::now_v7();
        let assistant_with_mcp = json!({
            "type": "assistant",
            "message": {
                "model": "claude-sonnet-4-6",
                "role": "assistant",
                "content": [{
                    "type": "tool_use",
                    "id": "toolu_mcp",
                    "name": "mcp__myserver__do_thing",
                    "input": {}
                }],
                "usage": { "input_tokens": 1, "output_tokens": 0 }
            },
            "timestamp": "2026-05-14T04:43:16Z"
        });
        let content = jsonl(&[
            user_record("call MCP", "2026-05-14T04:43:15Z"),
            assistant_with_mcp,
        ]);
        stage_session_file(home.path(), cwd.path(), session_id, &content);

        let result = load_claude_transcript(home.path(), cwd.path(), session_id, agent_id).unwrap();
        let Turn::Agent { items, .. } = &result.turns[1] else {
            panic!("expected Agent turn");
        };
        let TurnItem::Tool { kind, .. } = &items[0] else {
            panic!("expected Tool item");
        };
        assert_eq!(*kind, ToolKind::Mcp);
    }

    #[test]
    fn malformed_json_line_is_skipped_with_warning() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let session_id = Uuid::now_v7();
        let agent_id = Uuid::now_v7();
        let content = format!(
            "{}\n{{ not valid json\n{}",
            serde_json::to_string(&user_record("Say 1", "2026-05-14T04:43:15Z")).unwrap(),
            serde_json::to_string(&assistant_text_record(
                "1",
                "claude-sonnet-4-6",
                "2026-05-14T04:43:16Z"
            ))
            .unwrap()
        );
        stage_session_file(home.path(), cwd.path(), session_id, &content);

        let result = load_claude_transcript(home.path(), cwd.path(), session_id, agent_id).unwrap();
        assert_eq!(result.turns.len(), 2, "surrounding turns survive");
        assert_eq!(result.warnings.len(), 1);
        assert_eq!(result.warnings[0].line_number, 2);
    }

    #[test]
    fn empty_session_file_returns_empty_transcript() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let session_id = Uuid::now_v7();
        let agent_id = Uuid::now_v7();
        stage_session_file(home.path(), cwd.path(), session_id, "");
        let result = load_claude_transcript(home.path(), cwd.path(), session_id, agent_id).unwrap();
        assert!(result.turns.is_empty());
        assert!(result.warnings.is_empty());
    }

    #[test]
    fn fallback_scan_finds_session_under_different_encoded_directory() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let session_id = Uuid::now_v7();
        let agent_id = Uuid::now_v7();
        // Place the session file under an unrelated `projects/<dir>/` so the
        // primary-path canonicalization-based lookup misses it; fallback
        // scan should still find it by filename.
        let wrong_dir = home
            .path()
            .join(".claude")
            .join("projects")
            .join("-some-other-dir");
        std::fs::create_dir_all(&wrong_dir).unwrap();
        let content = jsonl(&[
            user_record("Say 1", "2026-05-14T04:43:15Z"),
            assistant_text_record("1", "claude-sonnet-4-6", "2026-05-14T04:43:16Z"),
        ]);
        std::fs::write(wrong_dir.join(format!("{session_id}.jsonl")), &content).unwrap();

        let result = load_claude_transcript(home.path(), cwd.path(), session_id, agent_id).unwrap();
        assert_eq!(result.turns.len(), 2, "fallback scan succeeds");
    }

    #[test]
    fn user_record_only_no_assistant_still_emits_user_turn() {
        // Defensive: a session file containing only a `user` record (e.g.,
        // the assistant response was never written) still surfaces the
        // user prompt.
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let session_id = Uuid::now_v7();
        let agent_id = Uuid::now_v7();
        let content = jsonl(&[user_record("Say 1", "2026-05-14T04:43:15Z")]);
        stage_session_file(home.path(), cwd.path(), session_id, &content);

        let result = load_claude_transcript(home.path(), cwd.path(), session_id, agent_id).unwrap();
        assert_eq!(result.turns.len(), 1);
        assert!(matches!(result.turns[0], Turn::User { .. }));
    }

    #[test]
    fn unknown_record_types_are_silently_skipped() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let session_id = Uuid::now_v7();
        let agent_id = Uuid::now_v7();
        let content = jsonl(&[
            json!({"type": "queue-operation", "operation": "enqueue", "content": "Say 1"}),
            json!({"type": "attachment", "attachment": {}}),
            json!({"type": "last-prompt", "lastPrompt": "Say 1"}),
            user_record("Say 1", "2026-05-14T04:43:15Z"),
            assistant_text_record("1", "claude-sonnet-4-6", "2026-05-14T04:43:16Z"),
        ]);
        stage_session_file(home.path(), cwd.path(), session_id, &content);

        let result = load_claude_transcript(home.path(), cwd.path(), session_id, agent_id).unwrap();
        assert_eq!(result.turns.len(), 2);
        assert!(result.warnings.is_empty());
    }

    #[test]
    fn first_assistant_model_is_used_for_meta() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let session_id = Uuid::now_v7();
        let agent_id = Uuid::now_v7();
        let content = jsonl(&[
            user_record("Say 1", "2026-05-14T04:43:15Z"),
            assistant_text_record("a", "claude-opus-4-7", "2026-05-14T04:43:16Z"),
            user_record("Say 2", "2026-05-14T04:43:17Z"),
            assistant_text_record("b", "claude-sonnet-4-6", "2026-05-14T04:43:18Z"),
        ]);
        stage_session_file(home.path(), cwd.path(), session_id, &content);

        let result = load_claude_transcript(home.path(), cwd.path(), session_id, agent_id).unwrap();
        let meta = result.meta.unwrap();
        assert_eq!(meta.model, "claude-opus-4-7", "first model wins");
    }
}
