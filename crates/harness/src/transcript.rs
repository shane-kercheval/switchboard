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

use crate::events::{ContentKind, McpServerStatus, ToolKind, TurnId, TurnUsage};

/// One reconstructed turn. Discriminated by `role` matching the event-vocabulary
/// convention. User turns carry just the prompt text; agent turns carry the
/// ordered `items` stream plus per-turn usage and lifecycle status.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "role", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Turn {
    User {
        turn_id: TurnId,
        agent_id: AgentId,
        started_at: DateTime<Utc>,
        text: String,
    },
    Agent {
        turn_id: TurnId,
        agent_id: AgentId,
        started_at: DateTime<Utc>,
        ended_at: Option<DateTime<Utc>>,
        status: TurnStatus,
        items: Vec<TurnItem>,
        usage: Option<TurnUsage>,
    },
}

/// Lifecycle state for an agent turn reconstructed from disk.
///
/// `Streaming` is reserved for the live path (in-flight turn); hydrated
/// transcripts never carry `Streaming`. `Failed` covers both genuine
/// harness-reported failures and truncated-mid-turn disk content (no terminal
/// event observed before EOF).
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
    #[error("I/O error reading session file: {0}")]
    Io(#[from] std::io::Error),
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
        };
        let value = serde_json::to_value(&turn).unwrap();
        assert_eq!(value["role"], "user");
        assert_eq!(value["text"], "hello");
        let parsed: Turn = serde_json::from_value(value).unwrap();
        assert_eq!(parsed, turn);
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
        };
        let value = serde_json::to_value(&turn).unwrap();
        assert_eq!(value["role"], "agent");
        assert_eq!(value["status"], "complete");
        assert_eq!(value["items"][0]["item_kind"], "text");
        assert_eq!(value["items"][1]["item_kind"], "tool");
        assert_eq!(value["items"][1]["tool_use_id"], "tool_1");
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
