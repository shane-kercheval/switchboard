//! Shared types for transcript hydration from harness session files.
//!
//! `Turn` and `TurnItem` mirror the TS shape in `src/lib/state/types.ts`
//! verbatim. `items: Vec<TurnItem>` is a single ordered stream of text +
//! tool entries — interleaved by arrival, not split into parallel arrays —
//! so chronology observable from the session file survives serde without
//! any boundary translation.
//!
//! `LoadedTranscript` is the output shape both per-harness parsers return.
//! Errors are limited to lookup-mechanism failures
//! ([`LoadTranscriptError`]); per-line parse damage degrades gracefully
//! via [`ParseWarning`] entries inside an otherwise-`Ok` result.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use switchboard_core::AgentId;

use crate::events::{ContentKind, McpServerStatus, ToolKind, TurnId, TurnSpend, TurnUsage};

/// Origin of a reconstructed user prompt. The conversation merge uses it to
/// decide whether the journal already owns this prompt (suppress the harness
/// copy) or it must render from the harness file (imported). Populated by the
/// Claude parser from the session file's `promptSource`; other harnesses leave
/// it `Unknown`, which routes their turns through the merge's count-based
/// fallback. **Backend-private** — never serialized to the IPC wire (the merge
/// consumes it in-process), mirroring `Turn::Agent.stable_message_id`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum UserPromptSource {
    /// Dispatched by an SDK/headless client (Switchboard) — Claude
    /// `promptSource: "sdk"`. The journal owns this prompt when a send pairs
    /// with it.
    Sdk,
    /// Typed into the harness's own TUI (`typed`/`queued`) — a genuine prompt
    /// the journal never saw; always renders imported.
    External,
    /// No provenance signal (older CLI, or a non-Claude harness). Routes the
    /// agent through the merge's count-based correlation fallback.
    #[default]
    Unknown,
}

/// One reconstructed turn. Discriminated by `role` matching the event-vocabulary
/// convention. User turns carry just the prompt text; agent turns carry the
/// ordered `items` stream plus per-turn usage and lifecycle status.
// A `User` turn is tiny; an `Agent` turn carries the full per-turn payload
// (items, usage, model/effort, spend). The asymmetry is intrinsic — boxing the
// hot, pattern-matched-everywhere `Agent` variant would add indirection for no
// real benefit (a `Turn` is short-lived and not stored in bulk arrays of mixed
// variants where the size waste would matter).
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "role", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Turn {
    User {
        turn_id: TurnId,
        agent_id: AgentId,
        started_at: DateTime<Utc>,
        text: String,
        /// Origin of this prompt (see [`UserPromptSource`]). Backend-private —
        /// `skip_serializing` keeps it off the IPC wire; `default` fills it as
        /// `Unknown` on any deserialize. Consumed only by the conversation merge.
        #[serde(default, skip_serializing)]
        source: UserPromptSource,
    },
    Agent {
        turn_id: TurnId,
        agent_id: AgentId,
        started_at: DateTime<Utc>,
        ended_at: Option<DateTime<Utc>>,
        status: TurnStatus,
        items: Vec<TurnItem>,
        usage: Option<TurnUsage>,
        /// The model this turn ran on, reconstructed from the harness session
        /// file (Codex `turn_context.model`, Claude `message.model`, Gemini's
        /// per-record `model`, Antigravity carry-forward). Per-turn *history* —
        /// distinct from the agent's *selected* model on `AgentRecord`, and from
        /// the agent-scoped `SessionMeta.model`. `None` when the harness exposes
        /// no model, or before any announcement (Antigravity).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        /// The reasoning effort this turn ran at — **Codex only**
        /// (`turn_context.effort`). `None` for Claude/Gemini/Antigravity, which
        /// expose no per-turn effort (Claude's is sidebar-intent only).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        effort: Option<String>,
        /// Per-turn real-spend attribution restored on reopen. The session-file
        /// parser leaves this `None`; the app's metadata overlay fills it from
        /// the durable `turnmeta` sidecar by joining on `stable_message_id`, so
        /// the inline cost/overage surface survives restart. `None` for
        /// non-real-spend turns and pre-feature turns (no backfill).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        spend: Option<TurnSpend>,
        /// **Stable hydration key** — a per-turn identity that survives
        /// re-parsing the *same* session file (byte-identical across repeated
        /// parses), so re-reading a file never duplicates this agent turn (user
        /// turns are keyless and dedup by `turn_id`). This is the
        /// frontend-facing sibling of `stable_message_id`: it is **serialized
        /// onto the IPC wire** (the merge on the other side dedups by it), and it
        /// carries a deliberately broader contract.
        ///
        /// Per harness: Claude the **first** non-subagent assistant `message.id`
        /// (matches the live `TurnEnd`); Codex the `event_msg/task_started.turn_id`;
        /// Gemini the turn's first `gemini` record `id`. `None` for Antigravity
        /// (no native per-turn id) — the merge falls back to `turn_id` for
        /// keyless turns.
        ///
        /// **Not** `stable_message_id`: that one stays private (`skip_serializing`,
        /// Claude-only cost-join) and is the **final** assistant `message.id`.
        /// The two **differ** for Claude on multi-assistant/tool-use turns by
        /// design — the first id is parse-invariant (stable from the turn's first
        /// output, so a mid-flight re-read dedups correctly) while the final id
        /// moves until the turn ends. Do not collapse them.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hydration_key: Option<String>,
        /// The final assistant message's Anthropic `message.id` (Claude only) —
        /// the join key the overlay uses to look up this turn's persisted
        /// cost/overage. Internal plumbing: set by the session-file parser,
        /// consumed by the app overlay (which runs before serialization), then
        /// **never serialized** — `skip_serializing` keeps it off the IPC wire so
        /// the frontend never sees a field the backend treats as private.
        #[serde(default, skip_serializing)]
        stable_message_id: Option<String>,
    },
}

/// Lifecycle state for an agent turn reconstructed from disk.
///
/// `Streaming` means the turn was **still in flight** when the file was read —
/// emitted live for an in-flight turn, and on disk for the EOF tail turn whose
/// last assistant `stop_reason` shows the model owed a continuation (Claude;
/// see `claude_code/session_file.rs::eof_tail_status`). A hydrated transcript
/// *can* therefore carry `Streaming` for its final turn; do not assume disk
/// turns are always terminal. `Failed` covers genuine harness-reported failures
/// and, for harnesses with a per-turn completion marker (Codex), truncated
/// mid-turn disk content where that marker is absent.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TurnStatus {
    Streaming,
    Complete,
    Failed,
}

/// One entry in an agent turn's `items` stream. Discriminated by `item_kind`
/// matching the TS shape. Text variants carry a `kind` (text vs thinking)
/// from the live-event vocabulary; tool variants carry start/completion
/// timestamps and the paired output (None until the matching completion
/// record is observed).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "item_kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum TurnItem {
    Text {
        kind: ContentKind,
        text: String,
    },
    Tool {
        tool_use_id: String,
        kind: ToolKind,
        name: String,
        input: serde_json::Value,
        output: Option<String>,
        is_error: Option<bool>,
        started_at: DateTime<Utc>,
        completed_at: Option<DateTime<Utc>>,
    },
}

/// Output of a `load_*_transcript` parser. All fields populate best-effort —
/// the parser drops malformed entries with warnings rather than failing the
/// whole load. Only lookup-mechanism failures raise [`LoadTranscriptError`].
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct LoadedTranscript {
    pub turns: Vec<Turn>,
    pub meta: Option<SessionMetaInfo>,
    pub last_rate_limit: Option<serde_json::Value>,
    /// Capture time of `last_rate_limit` when it was restored from the
    /// per-agent metadata sidecar (a stream-only/class-C value that would
    /// otherwise be lost on restart). Drives the UI's "as of …" staleness
    /// qualifier so a rehydrated snapshot isn't shown as live.
    ///
    /// **Always `None` from the per-harness loaders** — they have no
    /// `project_id` and don't read the metadata sidecar. It is populated only
    /// by the app-layer overlay (`load_agent_transcript`) after the load.
    /// Raw harness-crate consumers must not rely on this field being set; a
    /// class-B value (e.g. Codex's session-file rate-limit) carries `None`
    /// here because it's already durable and needs no staleness qualifier.
    pub last_rate_limit_as_of: Option<DateTime<Utc>>,
    pub warnings: Vec<ParseWarning>,
}

/// Session-scope metadata reconstructed from the session file + harness
/// config loaders. Mirrors the live `SessionMeta` event's payload so the
/// frontend reducer's hydrate handler can populate `runtimes[id].meta` from
/// either source identically.
///
/// `harness_version` may be empty on Claude (no on-disk analog of Codex's
/// `cli_version`); consumers tolerate empty strings as "absent" per the
/// existing live-path convention in `parse_system_event`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionMetaInfo {
    pub model: String,
    pub harness_version: String,
    pub tools: Vec<String>,
    pub mcp_servers: Vec<McpServerStatus>,
    pub skills: Vec<String>,
}

/// One per-line parse issue inside an otherwise-readable session file.
///
/// `line_number` is 1-based to match editor / tail-output conventions.
/// `reason` is human-readable so the UI banner can surface it verbatim
/// when the user investigates.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ParseWarning {
    pub line_number: usize,
    pub reason: String,
}

/// Errors returned by `load_*_transcript`. Reserved for lookup-mechanism
/// failures — I/O on a file that exists. **Not** raised for missing files
/// or per-line parse damage; those degrade silently to empty results /
/// warnings inside `LoadedTranscript`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LoadTranscriptError {
    /// Reading a session file that exists failed (permissions, mid-rotation,
    /// FS error). The `path` is included so the surfaced message names the file
    /// the user (or a bug report) can investigate — the OS error alone omits
    /// it. There is no `#[from] std::io::Error`: callers must attach the path
    /// at the read site, which is the whole point.
    #[error("I/O error reading session file {}: {source}", path.display())]
    Io {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Build a `ParseWarning` for a stale-sidecar case (sidecar exists, session
/// file at recorded path doesn't). Centralized so frontend / tests can
/// match the reason string exactly.
#[must_use]
pub fn stale_sidecar_warning() -> ParseWarning {
    ParseWarning {
        line_number: 0,
        reason: "session file no longer at recorded path".to_owned(),
    }
}

/// Compose a `SessionMetaInfo` from parser-extracted fields (`model`,
/// `harness_version`) and config-loader output (`mcp_servers`, `skills`).
/// Used by both per-harness `load_*_transcript` entry points to keep the
/// two-source merge identical across harnesses.
///
/// Parser-extracted fields are preserved verbatim. Config-loader output
/// is layered on top of whatever the parser found. `tools` is always empty
/// (no tools registry on disk for either harness; the live `system/init`
/// event is the only populator).
#[must_use]
pub fn merge_meta_with_loaders(
    parser_meta: Option<SessionMetaInfo>,
    mcp_servers: Vec<McpServerStatus>,
    skills: Vec<String>,
) -> SessionMetaInfo {
    let (model, harness_version) = parser_meta
        .map(|m| (m.model, m.harness_version))
        .unwrap_or_default();
    SessionMetaInfo {
        model,
        harness_version,
        tools: vec![],
        mcp_servers,
        skills,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use uuid::Uuid;

    fn fresh_turn_id() -> TurnId {
        Uuid::now_v7()
    }

    fn fresh_agent_id() -> AgentId {
        Uuid::now_v7()
    }

    #[test]
    fn user_turn_wire_shape() {
        let turn = Turn::User {
            turn_id: fresh_turn_id(),
            agent_id: fresh_agent_id(),
            started_at: Utc::now(),
            text: "hello".to_owned(),
            // A non-default source to prove it is dropped on serialize.
            source: UserPromptSource::Sdk,
        };
        let value = serde_json::to_value(&turn).unwrap();
        assert_eq!(value["role"], "user");
        assert_eq!(value["text"], "hello");
        // `source` is backend-private — never serialized onto the IPC wire.
        assert!(
            value.get("source").is_none(),
            "source must not cross the wire"
        );
        // The wire never carries it, so it deserializes as the `Unknown` default;
        // the public fields still round-trip.
        let Turn::User { source, text, .. } = serde_json::from_value(value).unwrap() else {
            panic!("expected a user turn");
        };
        assert_eq!(source, UserPromptSource::Unknown);
        assert_eq!(text, "hello");
    }

    #[test]
    fn agent_turn_with_interleaved_items_round_trips() {
        let started_at = Utc::now();
        let turn = Turn::Agent {
            turn_id: fresh_turn_id(),
            agent_id: fresh_agent_id(),
            started_at,
            ended_at: Some(started_at),
            status: TurnStatus::Complete,
            items: vec![
                TurnItem::Text {
                    kind: ContentKind::Text,
                    text: "thinking out loud".to_owned(),
                },
                TurnItem::Tool {
                    tool_use_id: "tool_1".to_owned(),
                    kind: ToolKind::Builtin,
                    name: "Bash".to_owned(),
                    input: json!({"command": "ls"}),
                    output: Some("a\nb\n".to_owned()),
                    is_error: Some(false),
                    started_at,
                    completed_at: Some(started_at),
                },
                TurnItem::Text {
                    kind: ContentKind::Text,
                    text: "done".to_owned(),
                },
            ],
            usage: None,
            spend: None,
            model: None,
            effort: None,
            hydration_key: Some("msg_anchor01".to_owned()),
            stable_message_id: None,
        };
        let value = serde_json::to_value(&turn).unwrap();
        assert_eq!(value["role"], "agent");
        assert_eq!(value["status"], "complete");
        assert_eq!(value["items"][0]["item_kind"], "text");
        assert_eq!(value["items"][1]["item_kind"], "tool");
        assert_eq!(value["items"][1]["tool_use_id"], "tool_1");
        // The hydration key serializes onto the wire (the frontend merge dedups
        // on it); the private cost-join `stable_message_id` does not.
        assert_eq!(value["hydration_key"], "msg_anchor01");
        assert!(value.get("stable_message_id").is_none());
        let parsed: Turn = serde_json::from_value(value).unwrap();
        assert_eq!(parsed, turn);
    }

    #[test]
    fn loaded_transcript_default_is_empty_and_serializable() {
        let loaded = LoadedTranscript::default();
        let value = serde_json::to_value(&loaded).unwrap();
        assert_eq!(value["turns"], json!([]));
        assert!(value["meta"].is_null());
        assert!(value["last_rate_limit"].is_null());
        assert_eq!(value["warnings"], json!([]));
    }

    #[test]
    fn io_error_display_names_the_path_and_os_error() {
        let err = LoadTranscriptError::Io {
            path: std::path::PathBuf::from("/tmp/sessions/abc.jsonl"),
            source: std::io::Error::new(std::io::ErrorKind::PermissionDenied, "permission denied"),
        };
        let msg = err.to_string();
        assert!(
            msg.contains("/tmp/sessions/abc.jsonl"),
            "message must name the session file path for a bug report: {msg}"
        );
        assert!(
            msg.contains("permission denied"),
            "message must carry the underlying OS error: {msg}"
        );
    }

    #[test]
    fn stale_sidecar_warning_has_stable_reason() {
        let w = stale_sidecar_warning();
        assert_eq!(w.line_number, 0);
        assert_eq!(w.reason, "session file no longer at recorded path");
    }

    #[test]
    fn merge_meta_with_loaders_uses_parser_fields_and_layers_loader_output() {
        let parser_meta = Some(SessionMetaInfo {
            model: "gpt-5.4".to_owned(),
            harness_version: "0.130.0".to_owned(),
            tools: vec!["should_be_dropped".to_owned()],
            mcp_servers: vec![],
            skills: vec![],
        });
        let mcp = vec![McpServerStatus {
            name: "tiddly".to_owned(),
            status: "configured".to_owned(),
        }];
        let skills = vec!["debug".to_owned()];
        let merged = merge_meta_with_loaders(parser_meta, mcp.clone(), skills.clone());
        assert_eq!(merged.model, "gpt-5.4");
        assert_eq!(merged.harness_version, "0.130.0");
        assert!(merged.tools.is_empty(), "tools always empty");
        assert_eq!(merged.mcp_servers, mcp);
        assert_eq!(merged.skills, skills);
    }

    #[test]
    fn merge_meta_with_loaders_handles_no_parser_contribution() {
        let merged = merge_meta_with_loaders(None, vec![], vec![]);
        assert!(merged.model.is_empty());
        assert!(merged.harness_version.is_empty());
        assert!(merged.mcp_servers.is_empty());
        assert!(merged.skills.is_empty());
    }
}
