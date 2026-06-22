//! Hydration: reconstruct an Antigravity conversation's prior turns from its
//! `transcript.jsonl` into the normalized [`LoadedTranscript`] the UI consumes
//! for every harness.
//!
//! Antigravity is **single-file-per-conversation**: a resume appends to the
//! same `transcript.jsonl` (the conversation UUID lives in the directory
//! name), so hydration reads one file — there is no Gemini-style per-resume
//! file fan-out to merge. The conversation UUID comes from the per-agent
//! sidecar (server-assigned, never `AgentRecord.session_id`, which is always
//! `None` for Antigravity).
//!
//! The reconstruction differs from the live path in one deliberate way: live
//! dispatch streams the model's final answer over stdout and the transcript
//! tail skips re-emitting it; hydration has no stdout to replay, so it **does**
//! emit `PLANNER_RESPONSE.content` as a `Text` item.

use std::collections::VecDeque;
use std::path::Path;

use chrono::{DateTime, Utc};
use switchboard_core::AgentId;
use uuid::Uuid;

use crate::events::{ContentKind, TurnId};
use crate::transcript::{
    LoadTranscriptError, LoadedTranscript, ParseWarning, SessionMetaInfo, Turn, TurnItem,
    TurnStatus, merge_meta_with_loaders,
};

use super::parser::{TranscriptRecord, classify_tool_kind};
use super::{extract_model_from_record, paths, user_request_body};

/// Load an Antigravity conversation's transcript into a [`LoadedTranscript`].
///
/// `conversation_id` comes from the agent's registry locator
/// (`SessionLocator::Uuid`) — Antigravity agents register with
/// `session_locator: None`, since the UUID is assigned server-side and captured
/// post-dispatch. Pass `None` for an agent that has **never dispatched** (no
/// locator yet): the result has empty turns but still carries the
/// loader-derived MCP / skills registries, so the sidebar populates the moment
/// the agent is selected — matching Codex's never-dispatched path.
///
/// **Missing-transcript case** (conversation exists only as encrypted
/// protobuf, or the locator points at a path that no longer exists): returns
/// `Ok` with empty turns and loader-derived meta, plus a single debug log —
/// degrading display, never blocking project open. Only an I/O error on a file
/// that *does* exist raises [`LoadTranscriptError`].
///
/// `cwd` is the agent's bound working directory, forwarded to the MCP / skills
/// loaders (currently user-scope only, so it is reserved for a future
/// workspace scope).
pub fn load_antigravity_transcript(
    home_dir: &Path,
    cwd: &Path,
    conversation_id: Option<Uuid>,
    agent_id: AgentId,
) -> Result<LoadedTranscript, LoadTranscriptError> {
    // Display-only registries, loaded once and layered onto whatever turns
    // (if any) the transcript yields.
    let mcp_servers = super::config::load_mcp_servers(home_dir, cwd);
    let skills = super::skills::load_skills(home_dir, cwd);

    let Some(conversation_id) = conversation_id else {
        return Ok(LoadedTranscript {
            meta: Some(merge_meta_with_loaders(None, mcp_servers, skills)),
            ..LoadedTranscript::default()
        });
    };

    let path = paths::transcript_path(home_dir, conversation_id);
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::debug!(
                %conversation_id,
                path = %path.display(),
                "antigravity: transcript file absent; hydrating empty (encrypted-only conversation or rotated path)"
            );
            return Ok(LoadedTranscript {
                meta: Some(merge_meta_with_loaders(None, mcp_servers, skills)),
                ..LoadedTranscript::default()
            });
        }
        Err(e) => return Err(LoadTranscriptError::Io { path, source: e }),
    };

    let mut transcript = parse_antigravity_transcript_content(&content, agent_id);
    // Layer the registries onto the parsed model — the same two-source merge
    // the other loaders use.
    transcript.meta = Some(merge_meta_with_loaders(
        transcript.meta.take(),
        mcp_servers,
        skills,
    ));
    Ok(transcript)
}

/// Parse `transcript.jsonl` content into a [`LoadedTranscript`] (no FS access).
/// Exposed `pub(crate)` so unit tests drive the reconstructor without staging a
/// temp file.
pub(crate) fn parse_antigravity_transcript_content(
    content: &str,
    agent_id: AgentId,
) -> LoadedTranscript {
    let mut recon = Reconstruction::new(agent_id);
    // Parse only complete (newline-terminated) lines. A trailing line with no
    // newline is an incomplete final write (a truncated file); it is dropped
    // without a warning, mirroring the live tail's partial-line handling.
    let complete = match content.rfind('\n') {
        Some(idx) => &content[..=idx],
        None => "",
    };
    for (idx, line) in complete.lines().enumerate() {
        let line_number = idx + 1;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<TranscriptRecord>(line) {
            Ok(rec) => recon.ingest(line_number, &rec),
            Err(e) => recon.warn(line_number, format!("malformed JSON: {e}")),
        }
    }
    recon.finalize()
}

/// In-progress reconstruction. Walks records in file order, opening an agent
/// turn lazily on the first `MODEL` record after a `USER_INPUT` and closing it
/// on the next `USER_INPUT` (or at EOF).
struct Reconstruction {
    agent_id: AgentId,
    turns: Vec<Turn>,
    model: Option<String>,
    /// Running carry-forward model for per-turn stamping. Antigravity announces
    /// the model (a `USER_SETTINGS_CHANGE` sentence) only on the turn it
    /// *changes*; unchanged turns inherit the last announced value. Updated
    /// last-wins on each announcing `USER_INPUT`, stamped on every `Turn::Agent`.
    /// Distinct from `model` (first-wins, agent-scoped `meta.model`). `None`
    /// before the first announcement.
    current_model: Option<String>,
    current: Option<AgentBuilder>,
    warnings: Vec<ParseWarning>,
    /// Carried forward so a record missing `created_at` inherits the previous
    /// record's timestamp rather than the wall clock — keeping reconstruction
    /// deterministic (the project's test rule forbids wall-clock dependence).
    /// Seeds at the Unix epoch (chrono's `DateTime::<Utc>::default()`).
    last_ts: DateTime<Utc>,
}

struct AgentBuilder {
    turn_id: TurnId,
    agent_id: AgentId,
    started_at: DateTime<Utc>,
    last_seen_at: DateTime<Utc>,
    ended_at: Option<DateTime<Utc>>,
    items: Vec<TurnItem>,
    /// Indices into `items` of `Tool` entries awaiting their result record,
    /// FIFO — Antigravity executes tools sequentially and result records carry
    /// no tool id, so order pairs them (same rule as the live path).
    pending_tools: VecDeque<usize>,
    saw_terminal: bool,
    failed: bool,
}

impl AgentBuilder {
    fn new(agent_id: AgentId, started_at: DateTime<Utc>) -> Self {
        Self {
            turn_id: Uuid::now_v7(),
            agent_id,
            started_at,
            last_seen_at: started_at,
            ended_at: None,
            items: Vec::new(),
            pending_tools: VecDeque::new(),
            saw_terminal: false,
            failed: false,
        }
    }
}

impl Reconstruction {
    fn new(agent_id: AgentId) -> Self {
        Self {
            agent_id,
            turns: Vec::new(),
            current: None,
            model: None,
            current_model: None,
            warnings: Vec::new(),
            last_ts: DateTime::<Utc>::default(),
        }
    }

    fn warn(&mut self, line_number: usize, reason: impl Into<String>) {
        self.warnings.push(ParseWarning {
            line_number,
            reason: reason.into(),
        });
    }

    /// Parse a record's timestamp leniently: an absent OR unparseable
    /// `created_at` carries the prior record's timestamp forward rather than
    /// dropping the record. A present-but-bad value also warns, so
    /// timestamp-format drift surfaces instead of silently degrading. Mirrors
    /// Codex's `parse_from_rfc3339(...).ok()` handling.
    fn resolve_timestamp(&mut self, line_number: usize, rec: &TranscriptRecord) -> DateTime<Utc> {
        let Some(raw) = rec.created_at.as_deref() else {
            return self.last_ts;
        };
        if let Ok(dt) = DateTime::parse_from_rfc3339(raw) {
            return dt.with_timezone(&Utc);
        }
        self.warn(
            line_number,
            format!("invalid created_at timestamp {raw:?}; carried prior timestamp forward"),
        );
        self.last_ts
    }

    #[allow(clippy::too_many_lines)]
    fn ingest(&mut self, line_number: usize, rec: &TranscriptRecord) {
        let ts = self.resolve_timestamp(line_number, rec);
        self.last_ts = ts;

        if rec.record_type == "USER_INPUT" {
            // A new user prompt closes the prior agent turn and opens a fresh
            // user turn. Boundary is the record type, not `step_index`
            // arithmetic (indices are non-contiguous — internal steps elided).
            self.close_current();
            let text = rec
                .content
                .as_deref()
                .and_then(user_request_body)
                .map(str::to_owned)
                .or_else(|| rec.content.clone())
                .unwrap_or_default();
            self.turns.push(Turn::User {
                turn_id: Uuid::now_v7(),
                agent_id: self.agent_id,
                started_at: ts,
                text,
                source: crate::transcript::UserPromptSource::Unknown,
            });
            // `close_current()` above already stamped the prior turn with the
            // old `current_model`; now fold in this turn's announcement (if any)
            // for the turn that follows. `model` stays first-wins (meta).
            if let Some(m) = extract_model_from_record(rec) {
                if self.model.is_none() {
                    self.model = Some(m.clone());
                }
                self.current_model = Some(m);
            }
            return;
        }

        // Non-MODEL internal steps (e.g. SYSTEM / CONVERSATION_HISTORY) carry
        // no user-visible content.
        if rec.source != "MODEL" {
            return;
        }

        let agent_id = self.agent_id;
        let mut builder = self
            .current
            .take()
            .unwrap_or_else(|| AgentBuilder::new(agent_id, ts));
        builder.last_seen_at = ts;
        let mut warning: Option<String> = None;

        if let Some(thinking) = &rec.thinking
            && !thinking.trim().is_empty()
        {
            builder.items.push(TurnItem::Text {
                kind: ContentKind::Thinking,
                text: thinking.clone(),
            });
        }

        if rec.is_planner_response() {
            if let Some(calls) = &rec.tool_calls {
                for (call_index, call) in calls.iter().enumerate() {
                    let tool_use_id = format!("{}:{}:{}", rec.step_index, call_index, call.name);
                    let idx = builder.items.len();
                    builder.items.push(TurnItem::Tool {
                        tool_use_id,
                        kind: classify_tool_kind(&call.name),
                        name: call.name.clone(),
                        input: call.args.clone(),
                        output: None,
                        is_error: None,
                        started_at: ts,
                        completed_at: None,
                    });
                    builder.pending_tools.push_back(idx);
                }
            }
            if rec.is_terminal_answer() {
                if let Some(content) = &rec.content {
                    builder.items.push(TurnItem::Text {
                        kind: ContentKind::Text,
                        text: content.clone(),
                    });
                }
                builder.saw_terminal = true;
                builder.ended_at = Some(ts);
            }
            if rec.status.as_deref() == Some("FAILED") {
                builder.failed = true;
                builder.ended_at = Some(ts);
            }
        } else if rec.is_tool_result() {
            let is_error = rec.status.as_deref() == Some("FAILED");
            let output = rec.content.clone().unwrap_or_default();
            if let Some(item_idx) = builder.pending_tools.pop_front() {
                if let Some(TurnItem::Tool {
                    output: out,
                    is_error: err,
                    completed_at: cat,
                    ..
                }) = builder.items.get_mut(item_idx)
                {
                    *out = Some(output);
                    *err = Some(is_error);
                    *cat = Some(ts);
                }
            } else {
                warning = Some(format!(
                    "tool result (step {}) had no matching tool call",
                    rec.step_index
                ));
            }
        }

        self.current = Some(builder);
        if let Some(w) = warning {
            self.warn(line_number, w);
        }
    }

    /// Close the open agent turn (if any). Status is `Complete` only when a
    /// terminal answer was observed and no failure record arrived; otherwise
    /// `Failed` — covering both harness-reported failures and a transcript
    /// truncated before its terminal record.
    fn close_current(&mut self) {
        let Some(b) = self.current.take() else {
            return;
        };
        let status = if b.failed {
            TurnStatus::Failed
        } else if b.saw_terminal {
            TurnStatus::Complete
        } else {
            TurnStatus::Failed
        };
        self.turns.push(Turn::Agent {
            turn_id: b.turn_id,
            agent_id: b.agent_id,
            started_at: b.started_at,
            ended_at: b.ended_at.or(Some(b.last_seen_at)),
            status,
            items: b.items,
            usage: None,
            // Per-turn model carried forward from the last announcement (the
            // model name embeds Antigravity's effort tier, so there's no
            // separate effort axis).
            model: self.current_model.clone(),
            effort: None,
            // Antigravity has no cost/overage, no `stable_message_id`, and no
            // native per-turn id at all — so no hydration key. The merge falls
            // back to `turn_id` for keyless turns; it's the one never-M3-eligible
            // harness.
            spend: None,
            hydration_key: None,
            stable_message_id: None,
        });
    }

    fn finalize(mut self) -> LoadedTranscript {
        self.close_current();
        LoadedTranscript {
            turns: self.turns,
            meta: Some(SessionMetaInfo {
                model: self.model.unwrap_or_default(),
                harness_version: String::new(),
                tools: vec![],
                mcp_servers: vec![],
                skills: vec![],
            }),
            last_rate_limit: None,
            last_rate_limit_as_of: None,
            warnings: self.warnings,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent_id() -> AgentId {
        Uuid::now_v7()
    }

    fn user_turn_text(turn: &Turn) -> &str {
        match turn {
            Turn::User { text, .. } => text,
            other => panic!("expected user turn, got {other:?}"),
        }
    }

    fn agent_items(turn: &Turn) -> &[TurnItem] {
        match turn {
            Turn::Agent { items, .. } => items,
            other => panic!("expected agent turn, got {other:?}"),
        }
    }

    fn agent_status(turn: &Turn) -> TurnStatus {
        match turn {
            Turn::Agent { status, .. } => *status,
            other => panic!("expected agent turn, got {other:?}"),
        }
    }

    fn agent_model(turn: &Turn) -> Option<String> {
        match turn {
            Turn::Agent { model, .. } => model.clone(),
            other => panic!("expected agent turn, got {other:?}"),
        }
    }

    #[test]
    fn per_turn_model_carries_forward_across_unannounced_turns() {
        // Turn 1 announces a model; turns 2–3 don't → inherit it; turn 4
        // announces a change → flips. Antigravity emits the
        // `USER_SETTINGS_CHANGE` sentence only when the model changes.
        let user_announce = |req: &str, change: &str, ts: &str| {
            format!(
                r#"{{"step_index":0,"source":"USER_EXPLICIT","type":"USER_INPUT","status":"DONE","created_at":"{ts}","content":"<USER_REQUEST>\n{req}\n</USER_REQUEST>\n<USER_SETTINGS_CHANGE>\nThe user changed setting `Model Selection` {change}.</USER_SETTINGS_CHANGE>"}}"#
            )
        };
        let user_plain = |req: &str, ts: &str| {
            format!(
                r#"{{"step_index":0,"source":"USER_EXPLICIT","type":"USER_INPUT","status":"DONE","created_at":"{ts}","content":"<USER_REQUEST>\n{req}\n</USER_REQUEST>"}}"#
            )
        };
        let model_resp = |text: &str, ts: &str| {
            format!(
                r#"{{"step_index":2,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","created_at":"{ts}","content":"{text}"}}"#
            )
        };
        let content = [
            user_announce(
                "one",
                "from None to Gemini 3.5 Flash (High)",
                "2026-05-19T19:00:00Z",
            ),
            model_resp("a", "2026-05-19T19:00:01Z"),
            user_plain("two", "2026-05-19T19:01:00Z"),
            model_resp("b", "2026-05-19T19:01:01Z"),
            user_plain("three", "2026-05-19T19:02:00Z"),
            model_resp("c", "2026-05-19T19:02:01Z"),
            user_announce(
                "four",
                "from Gemini 3.5 Flash (High) to Claude Sonnet 4.6 (Thinking)",
                "2026-05-19T19:03:00Z",
            ),
            model_resp("d", "2026-05-19T19:03:01Z"),
            String::new(), // trailing newline — the parser drops a non-terminated final line
        ]
        .join("\n");

        let t = parse_antigravity_transcript_content(&content, agent_id());
        let models: Vec<_> = t
            .turns
            .iter()
            .filter(|turn| matches!(turn, Turn::Agent { .. }))
            .map(agent_model)
            .collect();
        assert_eq!(
            models,
            vec![
                Some("Gemini 3.5 Flash".to_owned()),
                Some("Gemini 3.5 Flash".to_owned()),
                Some("Gemini 3.5 Flash".to_owned()),
                Some("Claude Sonnet 4.6".to_owned()),
            ]
        );
    }

    #[test]
    fn per_turn_model_is_none_when_never_announced() {
        // An attached conversation truncated before its first announcement →
        // no model on the turn, no error.
        let content = concat!(
            r#"{"step_index":0,"source":"USER_EXPLICIT","type":"USER_INPUT","status":"DONE","created_at":"2026-05-19T19:00:00Z","content":"<USER_REQUEST>\nhi\n</USER_REQUEST>"}"#,
            "\n",
            r#"{"step_index":2,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","created_at":"2026-05-19T19:00:01Z","content":"ack"}"#,
            "\n",
        );
        let t = parse_antigravity_transcript_content(content, agent_id());
        let agent = t
            .turns
            .iter()
            .find(|x| matches!(x, Turn::Agent { .. }))
            .unwrap();
        assert_eq!(agent_model(agent), None);
    }

    #[test]
    fn empty_content_produces_no_turns_but_meta() {
        let t = parse_antigravity_transcript_content("", agent_id());
        assert!(t.turns.is_empty());
        assert!(t.meta.is_some());
        assert!(t.warnings.is_empty());
    }

    #[test]
    fn single_turn_user_and_agent_with_answer() {
        let content = concat!(
            r#"{"step_index":0,"source":"USER_EXPLICIT","type":"USER_INPUT","status":"DONE","created_at":"2026-05-19T19:19:59Z","content":"<USER_REQUEST>\nReply with ack\n</USER_REQUEST>\n<USER_SETTINGS_CHANGE>\nThe user changed setting `Model Selection` from None to Gemini 3.5 Flash (High). x</USER_SETTINGS_CHANGE>"}"#,
            "\n",
            r#"{"step_index":1,"source":"SYSTEM","type":"CONVERSATION_HISTORY","status":"DONE","created_at":"2026-05-19T19:19:59Z"}"#,
            "\n",
            r#"{"step_index":2,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","created_at":"2026-05-19T19:20:01Z","thinking":"deliberating","content":"ack"}"#,
            "\n",
        );
        let t = parse_antigravity_transcript_content(content, agent_id());
        assert_eq!(t.turns.len(), 2, "one user + one agent turn");
        assert_eq!(user_turn_text(&t.turns[0]), "Reply with ack");
        let items = agent_items(&t.turns[1]);
        // thinking item then the final answer text.
        assert_eq!(items.len(), 2);
        assert!(matches!(
            &items[0],
            TurnItem::Text { kind: ContentKind::Thinking, text } if text == "deliberating"
        ));
        assert!(matches!(
            &items[1],
            TurnItem::Text { kind: ContentKind::Text, text } if text == "ack"
        ));
        assert_eq!(agent_status(&t.turns[1]), TurnStatus::Complete);
        assert_eq!(t.meta.unwrap().model, "Gemini 3.5 Flash");
    }

    #[test]
    fn tool_call_then_result_pairs_and_completes() {
        let content = concat!(
            r#"{"step_index":0,"source":"USER_EXPLICIT","type":"USER_INPUT","status":"DONE","created_at":"2026-05-19T19:23:03Z","content":"<USER_REQUEST>\nlist files\n</USER_REQUEST>"}"#,
            "\n",
            r#"{"step_index":2,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","created_at":"2026-05-19T19:23:05Z","tool_calls":[{"name":"run_command","args":{"CommandLine":"\"ls\""}}]}"#,
            "\n",
            r#"{"step_index":3,"source":"MODEL","type":"RUN_COMMAND","status":"DONE","created_at":"2026-05-19T19:23:07Z","content":"Output:\nMARKER.txt\n"}"#,
            "\n",
            r#"{"step_index":4,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","created_at":"2026-05-19T19:23:08Z","content":"MARKER.txt"}"#,
            "\n",
        );
        let t = parse_antigravity_transcript_content(content, agent_id());
        let items = agent_items(&t.turns[1]);
        assert_eq!(items.len(), 2, "one tool + the final answer");
        match &items[0] {
            TurnItem::Tool {
                name,
                output,
                is_error,
                completed_at,
                ..
            } => {
                assert_eq!(name, "run_command");
                assert_eq!(output.as_deref(), Some("Output:\nMARKER.txt\n"));
                assert_eq!(*is_error, Some(false));
                assert!(completed_at.is_some(), "result record completed the tool");
            }
            other => panic!("expected tool item, got {other:?}"),
        }
        assert_eq!(agent_status(&t.turns[1]), TurnStatus::Complete);
    }

    #[test]
    fn multi_turn_segments_on_user_input() {
        let content = concat!(
            r#"{"step_index":0,"source":"USER_EXPLICIT","type":"USER_INPUT","status":"DONE","created_at":"2026-05-19T19:00:00Z","content":"<USER_REQUEST>\nfirst\n</USER_REQUEST>"}"#,
            "\n",
            r#"{"step_index":2,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","created_at":"2026-05-19T19:00:01Z","content":"answer one"}"#,
            "\n",
            r#"{"step_index":3,"source":"USER_EXPLICIT","type":"USER_INPUT","status":"DONE","created_at":"2026-05-19T19:05:00Z","content":"<USER_REQUEST>\nsecond\n</USER_REQUEST>"}"#,
            "\n",
            r#"{"step_index":5,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","created_at":"2026-05-19T19:05:02Z","content":"answer two"}"#,
            "\n",
        );
        let t = parse_antigravity_transcript_content(content, agent_id());
        assert_eq!(t.turns.len(), 4, "user/agent x2 interleaved in order");
        assert_eq!(user_turn_text(&t.turns[0]), "first");
        assert_eq!(user_turn_text(&t.turns[2]), "second");
        assert!(matches!(
            &agent_items(&t.turns[1])[0],
            TurnItem::Text { text, .. } if text == "answer one"
        ));
        assert!(matches!(
            &agent_items(&t.turns[3])[0],
            TurnItem::Text { text, .. } if text == "answer two"
        ));
    }

    #[test]
    fn unknown_record_type_treated_as_tool_result_and_continues() {
        let content = concat!(
            r#"{"step_index":0,"source":"USER_EXPLICIT","type":"USER_INPUT","status":"DONE","created_at":"2026-05-19T19:00:00Z","content":"<USER_REQUEST>\ngrep\n</USER_REQUEST>"}"#,
            "\n",
            r#"{"step_index":2,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","created_at":"2026-05-19T19:00:01Z","tool_calls":[{"name":"grep_search","args":{}}]}"#,
            "\n",
            r#"{"step_index":3,"source":"MODEL","type":"CortexStepGrepSearch","status":"DONE","created_at":"2026-05-19T19:00:02Z","content":"3 matches"}"#,
            "\n",
            r#"{"step_index":4,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","created_at":"2026-05-19T19:00:03Z","content":"found 3"}"#,
            "\n",
        );
        let t = parse_antigravity_transcript_content(content, agent_id());
        let items = agent_items(&t.turns[1]);
        match &items[0] {
            TurnItem::Tool { output, .. } => assert_eq!(output.as_deref(), Some("3 matches")),
            other => {
                panic!("expected tool item completed by the unknown CortexStep type, got {other:?}")
            }
        }
        assert_eq!(agent_status(&t.turns[1]), TurnStatus::Complete);
    }

    #[test]
    fn truncated_final_line_is_skipped_and_turn_is_failed() {
        // A turn whose terminal record never finished writing: the partial
        // trailing line (no newline) is dropped, and the agent turn has no
        // terminal answer → Failed.
        let content = concat!(
            r#"{"step_index":0,"source":"USER_EXPLICIT","type":"USER_INPUT","status":"DONE","created_at":"2026-05-19T19:00:00Z","content":"<USER_REQUEST>\nhi\n</USER_REQUEST>"}"#,
            "\n",
            r#"{"step_index":2,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","created_at":"2026-05-19T19:00:01Z","tool_calls":[{"name":"run_command","args":{}}]}"#,
            "\n",
            r#"{"step_index":3,"source":"MODEL","type":"RUN_COMMAND","status":"DONE","created_at":"2026-05-19T19:00:02Z","content":"part"#,
        );
        let t = parse_antigravity_transcript_content(content, agent_id());
        assert_eq!(t.turns.len(), 2);
        // The tool started but its result line was truncated away.
        let items = agent_items(&t.turns[1]);
        assert_eq!(items.len(), 1);
        assert!(matches!(
            &items[0],
            TurnItem::Tool {
                completed_at: None,
                output: None,
                ..
            }
        ));
        assert_eq!(agent_status(&t.turns[1]), TurnStatus::Failed);
        assert!(
            t.warnings.is_empty(),
            "partial line is silent, not a warning"
        );
    }

    #[test]
    fn failed_planner_response_marks_turn_failed() {
        let content = concat!(
            r#"{"step_index":0,"source":"USER_EXPLICIT","type":"USER_INPUT","status":"DONE","created_at":"2026-05-19T19:00:00Z","content":"<USER_REQUEST>\nhi\n</USER_REQUEST>"}"#,
            "\n",
            r#"{"step_index":2,"source":"MODEL","type":"PLANNER_RESPONSE","status":"FAILED","created_at":"2026-05-19T19:00:01Z","content":"the model errored"}"#,
            "\n",
        );
        let t = parse_antigravity_transcript_content(content, agent_id());
        assert_eq!(agent_status(&t.turns[1]), TurnStatus::Failed);
    }

    #[test]
    fn malformed_json_line_warns_and_continues() {
        let content = concat!(
            r#"{"step_index":0,"source":"USER_EXPLICIT","type":"USER_INPUT","status":"DONE","created_at":"2026-05-19T19:00:00Z","content":"<USER_REQUEST>\nhi\n</USER_REQUEST>"}"#,
            "\n",
            "{not valid json}",
            "\n",
            r#"{"step_index":2,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","created_at":"2026-05-19T19:00:01Z","content":"ack"}"#,
            "\n",
        );
        let t = parse_antigravity_transcript_content(content, agent_id());
        assert_eq!(t.warnings.len(), 1);
        assert_eq!(t.warnings[0].line_number, 2);
        assert_eq!(t.turns.len(), 2, "valid records still reconstruct");
    }

    #[test]
    fn malformed_created_at_preserves_record_warns_and_carries_timestamp() {
        // A present-but-unparseable timestamp must NOT drop the record (which
        // would lose the model's answer); it degrades to the prior timestamp
        // and warns so format drift is visible.
        let content = concat!(
            r#"{"step_index":0,"source":"USER_EXPLICIT","type":"USER_INPUT","status":"DONE","created_at":"2026-05-19T19:00:00Z","content":"<USER_REQUEST>\nhi\n</USER_REQUEST>"}"#,
            "\n",
            r#"{"step_index":2,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","created_at":"not-a-timestamp","content":"ack"}"#,
            "\n",
        );
        let t = parse_antigravity_transcript_content(content, agent_id());
        assert_eq!(
            t.turns.len(),
            2,
            "the answer record survives the bad timestamp"
        );
        assert!(matches!(
            &agent_items(&t.turns[1])[0],
            TurnItem::Text { text, .. } if text == "ack"
        ));
        assert_eq!(t.warnings.len(), 1);
        assert!(
            t.warnings[0].reason.contains("invalid created_at"),
            "warning names the timestamp, not 'malformed JSON': {}",
            t.warnings[0].reason
        );
        let expected: DateTime<Utc> = "2026-05-19T19:00:00Z".parse().unwrap();
        match &t.turns[1] {
            Turn::Agent { started_at, .. } => assert_eq!(*started_at, expected),
            other => panic!("expected agent turn, got {other:?}"),
        }
    }

    #[test]
    fn tool_result_without_preceding_call_warns_and_continues() {
        // A corrupt transcript: a tool-result record with no matching tool
        // call. The reconstruction recovers (turn still produced) and records a
        // warning rather than panicking or dropping the turn.
        let content = concat!(
            r#"{"step_index":0,"source":"USER_EXPLICIT","type":"USER_INPUT","status":"DONE","created_at":"2026-05-19T19:00:00Z","content":"<USER_REQUEST>\nhi\n</USER_REQUEST>"}"#,
            "\n",
            r#"{"step_index":2,"source":"MODEL","type":"RUN_COMMAND","status":"DONE","created_at":"2026-05-19T19:00:01Z","content":"orphan output"}"#,
            "\n",
            r#"{"step_index":3,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","created_at":"2026-05-19T19:00:02Z","content":"done"}"#,
            "\n",
        );
        let t = parse_antigravity_transcript_content(content, agent_id());
        assert_eq!(t.turns.len(), 2);
        assert_eq!(t.warnings.len(), 1);
        assert!(
            t.warnings[0].reason.contains("no matching tool call"),
            "warning explains the unmatched result: {}",
            t.warnings[0].reason
        );
        assert_eq!(agent_status(&t.turns[1]), TurnStatus::Complete);
    }

    #[test]
    fn missing_created_at_carries_prior_timestamp_forward() {
        // Second record omits created_at → it must inherit the first record's
        // timestamp, not the wall clock (deterministic).
        let content = concat!(
            r#"{"step_index":0,"source":"USER_EXPLICIT","type":"USER_INPUT","status":"DONE","created_at":"2026-05-19T19:00:00Z","content":"<USER_REQUEST>\nhi\n</USER_REQUEST>"}"#,
            "\n",
            r#"{"step_index":2,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","content":"ack"}"#,
            "\n",
        );
        let t = parse_antigravity_transcript_content(content, agent_id());
        let expected: DateTime<Utc> = "2026-05-19T19:00:00Z".parse().unwrap();
        match &t.turns[1] {
            Turn::Agent {
                started_at,
                ended_at,
                ..
            } => {
                assert_eq!(*started_at, expected);
                assert_eq!(*ended_at, Some(expected));
            }
            other => panic!("expected agent turn, got {other:?}"),
        }
    }
}
