use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use switchboard_core::AgentId;
use uuid::Uuid;

/// UUID v7 turn identifier — consistent with `AgentId` and `ProjectId`.
pub type TurnId = Uuid;

/// Tells the reducer / UI which content rendering applies to a chunk.
///
/// `Thinking` is reserved but not currently emitted — keeping the variant
/// in the vocabulary now means future reasoning UI lands without a
/// wire-format break.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ContentKind {
    /// User-facing assistant text.
    Text,
    /// Model thinking blocks. Reserved; not currently emitted.
    Thinking,
}

/// Discriminates tool origin so the UI can label calls (built-in tool vs MCP
/// vs plugin) without scraping the name. `Plugin` and `Other` are reserved
/// but not currently emitted — same forward-compat pattern as
/// `ContentKind::Thinking`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ToolKind {
    Builtin,
    Mcp,
    Plugin,
    Other,
}

/// One MCP server entry from a `SessionMeta` event. `status` is intentionally
/// an opaque string ("connected" / "disconnected" / "failed" / "needs-auth" /
/// future values) so a new harness-side status doesn't break deserialization.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpServerStatus {
    pub name: String,
    pub status: String,
}

/// Per-turn usage and cost. Carried on `TurnEnd.usage`.
///
/// `total_cost_usd` is Claude Code only (subscription auth has no dollar
/// number for Codex). `context_window` for Claude comes from
/// `result.modelUsage.<model>.contextWindow`; for Codex it's populated by
/// post-terminal session-file enrichment. All other fields are tokens.
///
/// Populated when the harness reports usage on its terminal event. Claude
/// carries it on both `Completed` and `Failed` turns; Codex carries it on
/// `Completed` only (Codex's `turn.failed` doesn't include `usage` in
/// observed fixtures). `usage: None` means "telemetry was unparseable /
/// absent," distinct from a real `Some` carrying zero values (which
/// Claude's synthetic auth-failure responses legitimately emit).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TurnUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_input_tokens: Option<u64>,
    pub reasoning_output_tokens: Option<u64>,
    pub context_window: Option<u32>,
    pub total_cost_usd: Option<f64>,
}

/// Events emitted by harness adapters. `TurnStart` is deliberately absent — it is
/// dispatcher-owned and synthesized before the stream is established. Excluding
/// it here makes the invariant type-enforced: no adapter author can accidentally emit it.
///
/// Variant scope: `ContentChunk`, `ToolStarted`, `ToolCompleted`, `TurnEnd` are
/// turn-scoped (the `turn_id` self-discriminates the agent via the transcript
/// map). `SessionMeta`, `RateLimitEvent` are agent-scoped (no turn anchor) and
/// carry `agent_id` in the payload so they're self-describing for logs,
/// persistence, and any future non-channel transport.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum AdapterEvent {
    ContentChunk {
        turn_id: TurnId,
        kind: ContentKind,
        text: String,
    },
    ToolStarted {
        turn_id: TurnId,
        tool_use_id: String,
        kind: ToolKind,
        name: String,
        input: serde_json::Value,
    },
    ToolCompleted {
        turn_id: TurnId,
        tool_use_id: String,
        output: String,
        is_error: bool,
    },
    TurnEnd {
        turn_id: TurnId,
        outcome: TurnOutcome,
        ended_at: DateTime<Utc>,
        usage: Option<TurnUsage>,
    },
    RateLimitEvent {
        agent_id: AgentId,
        info: serde_json::Value,
    },
    SessionMeta {
        agent_id: AgentId,
        model: String,
        harness_version: String,
        tools: Vec<String>,
        mcp_servers: Vec<McpServerStatus>,
        skills: Vec<String>,
        raw: serde_json::Value,
    },
}

/// Wire format across the IPC boundary to the frontend.
///
/// **Variant sources.** `TurnStart` and `AgentIdle` are dispatcher-owned —
/// neither exists on [`AdapterEvent`]. The dispatcher synthesizes them at
/// well-defined lifecycle points: `TurnStart` before the adapter stream is
/// established, `AgentIdle` after the stream drains and immediately before
/// the dispatcher's `AgentIdleGuard` drops. Adapter events lift into the
/// remaining variants via `From<AdapterEvent>`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum NormalizedEvent {
    TurnStart {
        turn_id: TurnId,
        started_at: DateTime<Utc>,
    },
    ContentChunk {
        turn_id: TurnId,
        kind: ContentKind,
        text: String,
    },
    ToolStarted {
        turn_id: TurnId,
        tool_use_id: String,
        kind: ToolKind,
        name: String,
        input: serde_json::Value,
    },
    ToolCompleted {
        turn_id: TurnId,
        tool_use_id: String,
        output: String,
        is_error: bool,
    },
    TurnEnd {
        turn_id: TurnId,
        outcome: TurnOutcome,
        ended_at: DateTime<Utc>,
        usage: Option<TurnUsage>,
    },
    RateLimitEvent {
        agent_id: AgentId,
        info: serde_json::Value,
    },
    SessionMeta {
        agent_id: AgentId,
        model: String,
        harness_version: String,
        tools: Vec<String>,
        mcp_servers: Vec<McpServerStatus>,
        skills: Vec<String>,
        raw: serde_json::Value,
    },
    /// Emitted by the dispatcher as the **last event on the per-agent
    /// channel** for a dispatch — immediately before `AgentIdleGuard`
    /// drops.
    ///
    /// Frontend consumers may rely on two contracts:
    ///
    /// 1. **Channel-ordering**: no further events arrive on this channel
    ///    for this dispatch.
    /// 2. **Sendability**: by the time the frontend processes `AgentIdle`,
    ///    the dispatcher accepts a new send to this agent without
    ///    returning `Busy`.
    ///
    /// Distinct from `TurnEnd`: `TurnEnd` is terminal for a turn,
    /// `AgentIdle` is terminal for the per-agent channel for this dispatch.
    /// For Codex agents, post-`TurnEnd` enrichment events
    /// (`RateLimitEvent`, `SessionMeta`) flow between `TurnEnd` and
    /// `AgentIdle`.
    AgentIdle { agent_id: AgentId },
}

impl From<AdapterEvent> for NormalizedEvent {
    fn from(e: AdapterEvent) -> Self {
        match e {
            AdapterEvent::ContentChunk {
                turn_id,
                kind,
                text,
            } => NormalizedEvent::ContentChunk {
                turn_id,
                kind,
                text,
            },
            AdapterEvent::ToolStarted {
                turn_id,
                tool_use_id,
                kind,
                name,
                input,
            } => NormalizedEvent::ToolStarted {
                turn_id,
                tool_use_id,
                kind,
                name,
                input,
            },
            AdapterEvent::ToolCompleted {
                turn_id,
                tool_use_id,
                output,
                is_error,
            } => NormalizedEvent::ToolCompleted {
                turn_id,
                tool_use_id,
                output,
                is_error,
            },
            AdapterEvent::TurnEnd {
                turn_id,
                outcome,
                ended_at,
                usage,
            } => NormalizedEvent::TurnEnd {
                turn_id,
                outcome,
                ended_at,
                usage,
            },
            AdapterEvent::RateLimitEvent { agent_id, info } => {
                NormalizedEvent::RateLimitEvent { agent_id, info }
            }
            AdapterEvent::SessionMeta {
                agent_id,
                model,
                harness_version,
                tools,
                mcp_servers,
                skills,
                raw,
            } => NormalizedEvent::SessionMeta {
                agent_id,
                model,
                harness_version,
                tools,
                mcp_servers,
                skills,
                raw,
            },
        }
    }
}

/// Outcome of a completed turn. The `kind` field on `Failed` is load-bearing
/// for retry policy: `HarnessError` (model/API issue) vs `AdapterFailure`
/// (subprocess crash, parse error, infrastructure) have different retry
/// semantics.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "status", rename_all = "snake_case")]
#[non_exhaustive]
pub enum TurnOutcome {
    Completed,
    Failed { kind: FailureKind, message: String },
}

/// Discriminates the cause of a failed turn for retry-policy decisions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum FailureKind {
    /// Harness reported `is_error: true` in its terminal `result` event.
    /// Caused by model/API issues (bad model name, rate limit, invalid prompt).
    HarnessError,
    /// Adapter synthesized this: subprocess died, parser hit malformed JSON, or
    /// stdout EOF arrived without a terminal `result` event. Typically transient.
    AdapterFailure,
    /// Subscription / tier auth is missing or expired. Detected via stream-event
    /// patterns: Claude's `assistant.error == "authentication_failed"` (state-flag
    /// pattern consumed in `parse_result`) or Codex's `turn.failed.error.message`
    /// containing `"401 Unauthorized"`. Frontend renders a "subscription auth
    /// required" banner rather than a generic error.
    AuthFailure,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fresh_turn_id() -> TurnId {
        Uuid::now_v7()
    }

    fn fresh_agent_id() -> AgentId {
        Uuid::now_v7()
    }

    fn fresh_time() -> DateTime<Utc> {
        Utc::now()
    }

    #[test]
    fn turn_start_wire_shape() {
        let turn_id = fresh_turn_id();
        let started_at = fresh_time();
        let event = NormalizedEvent::TurnStart {
            turn_id,
            started_at,
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], "turn_start");
        assert_eq!(value["turn_id"], turn_id.to_string());
        let parsed: NormalizedEvent = serde_json::from_value(value).unwrap();
        assert_eq!(parsed, event);
    }

    #[test]
    fn content_chunk_wire_shape_with_kind_text() {
        let turn_id = fresh_turn_id();
        let event = NormalizedEvent::ContentChunk {
            turn_id,
            kind: ContentKind::Text,
            text: "hello".to_owned(),
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], "content_chunk");
        assert_eq!(value["kind"], "text");
        assert_eq!(value["text"], "hello");
        let parsed: NormalizedEvent = serde_json::from_value(value).unwrap();
        assert_eq!(parsed, event);
    }

    #[test]
    fn content_chunk_wire_shape_preserves_thinking_kind() {
        let event = NormalizedEvent::ContentChunk {
            turn_id: fresh_turn_id(),
            kind: ContentKind::Thinking,
            text: "deliberating".to_owned(),
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["kind"], "thinking");
        let parsed: NormalizedEvent = serde_json::from_value(value).unwrap();
        assert_eq!(parsed, event);
    }

    #[test]
    fn turn_end_completed_wire_shape_with_no_usage() {
        let turn_id = fresh_turn_id();
        let event = NormalizedEvent::TurnEnd {
            turn_id,
            outcome: TurnOutcome::Completed,
            ended_at: fresh_time(),
            usage: None,
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], "turn_end");
        assert_eq!(value["outcome"]["status"], "completed");
        assert!(value["usage"].is_null());
        let parsed: NormalizedEvent = serde_json::from_value(value).unwrap();
        assert_eq!(parsed, event);
    }

    #[test]
    fn turn_end_wire_shape_with_full_usage() {
        let event = NormalizedEvent::TurnEnd {
            turn_id: fresh_turn_id(),
            outcome: TurnOutcome::Completed,
            ended_at: fresh_time(),
            usage: Some(TurnUsage {
                input_tokens: 100,
                output_tokens: 25,
                cached_input_tokens: Some(50),
                reasoning_output_tokens: Some(5),
                context_window: Some(200_000),
                total_cost_usd: Some(0.0125),
            }),
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["usage"]["input_tokens"], 100);
        assert_eq!(value["usage"]["output_tokens"], 25);
        assert_eq!(value["usage"]["cached_input_tokens"], 50);
        assert_eq!(value["usage"]["reasoning_output_tokens"], 5);
        assert_eq!(value["usage"]["context_window"], 200_000);
        assert_eq!(value["usage"]["total_cost_usd"], 0.0125);
        let parsed: NormalizedEvent = serde_json::from_value(value).unwrap();
        assert_eq!(parsed, event);
    }

    #[test]
    fn turn_end_failed_wire_shape() {
        let event = NormalizedEvent::TurnEnd {
            turn_id: fresh_turn_id(),
            outcome: TurnOutcome::Failed {
                kind: FailureKind::HarnessError,
                message: "bad model".to_owned(),
            },
            ended_at: fresh_time(),
            usage: None,
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["outcome"]["status"], "failed");
        assert_eq!(value["outcome"]["kind"], "harness_error");
        assert_eq!(value["outcome"]["message"], "bad model");
        let parsed: NormalizedEvent = serde_json::from_value(value).unwrap();
        assert_eq!(parsed, event);
    }

    #[test]
    fn auth_failure_kind_wire_shape() {
        let event = NormalizedEvent::TurnEnd {
            turn_id: fresh_turn_id(),
            outcome: TurnOutcome::Failed {
                kind: FailureKind::AuthFailure,
                message: "Not logged in · Please run /login".to_owned(),
            },
            ended_at: fresh_time(),
            usage: None,
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["outcome"]["status"], "failed");
        assert_eq!(value["outcome"]["kind"], "auth_failure");
        assert_eq!(
            value["outcome"]["message"],
            "Not logged in · Please run /login"
        );
        let parsed: NormalizedEvent = serde_json::from_value(value).unwrap();
        assert_eq!(parsed, event);
    }

    #[test]
    fn adapter_failure_kind_wire_shape() {
        let event = NormalizedEvent::TurnEnd {
            turn_id: fresh_turn_id(),
            outcome: TurnOutcome::Failed {
                kind: FailureKind::AdapterFailure,
                message: "crash".to_owned(),
            },
            ended_at: fresh_time(),
            usage: None,
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["outcome"]["kind"], "adapter_failure");
    }

    #[test]
    fn tool_started_wire_shape() {
        let event = NormalizedEvent::ToolStarted {
            turn_id: fresh_turn_id(),
            tool_use_id: "toolu_abc".to_owned(),
            kind: ToolKind::Builtin,
            name: "Bash".to_owned(),
            input: json!({"command": "ls"}),
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], "tool_started");
        assert_eq!(value["tool_use_id"], "toolu_abc");
        assert_eq!(value["kind"], "builtin");
        assert_eq!(value["name"], "Bash");
        assert_eq!(value["input"]["command"], "ls");
        let parsed: NormalizedEvent = serde_json::from_value(value).unwrap();
        assert_eq!(parsed, event);
    }

    #[test]
    fn tool_started_mcp_kind_wire_shape() {
        let event = NormalizedEvent::ToolStarted {
            turn_id: fresh_turn_id(),
            tool_use_id: "toolu_xyz".to_owned(),
            kind: ToolKind::Mcp,
            name: "mcp__server__action".to_owned(),
            input: json!({}),
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["kind"], "mcp");
        let parsed: NormalizedEvent = serde_json::from_value(value).unwrap();
        assert_eq!(parsed, event);
    }

    #[test]
    fn tool_completed_wire_shape() {
        let event = NormalizedEvent::ToolCompleted {
            turn_id: fresh_turn_id(),
            tool_use_id: "toolu_abc".to_owned(),
            output: "hello\n".to_owned(),
            is_error: false,
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], "tool_completed");
        assert_eq!(value["tool_use_id"], "toolu_abc");
        assert_eq!(value["output"], "hello\n");
        assert_eq!(value["is_error"], false);
        let parsed: NormalizedEvent = serde_json::from_value(value).unwrap();
        assert_eq!(parsed, event);
    }

    #[test]
    fn rate_limit_event_wire_shape() {
        let event = NormalizedEvent::RateLimitEvent {
            agent_id: fresh_agent_id(),
            info: json!({"status": "allowed", "resetsAt": 1_778_701_800u64}),
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], "rate_limit_event");
        assert_eq!(value["info"]["status"], "allowed");
        let parsed: NormalizedEvent = serde_json::from_value(value).unwrap();
        assert_eq!(parsed, event);
    }

    #[test]
    fn agent_idle_wire_shape() {
        let agent_id = fresh_agent_id();
        let event = NormalizedEvent::AgentIdle { agent_id };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], "agent_idle");
        assert_eq!(value["agent_id"], agent_id.to_string());
        let parsed: NormalizedEvent = serde_json::from_value(value).unwrap();
        assert_eq!(parsed, event);
    }

    #[test]
    fn session_meta_wire_shape() {
        let event = NormalizedEvent::SessionMeta {
            agent_id: fresh_agent_id(),
            model: "claude-sonnet-4-6".to_owned(),
            harness_version: "2.1.140".to_owned(),
            tools: vec!["Bash".to_owned(), "Read".to_owned()],
            mcp_servers: vec![McpServerStatus {
                name: "tiddly".to_owned(),
                status: "connected".to_owned(),
            }],
            skills: vec!["debug".to_owned()],
            raw: json!({"subtype": "init", "cwd": "/tmp"}),
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], "session_meta");
        assert_eq!(value["model"], "claude-sonnet-4-6");
        assert_eq!(value["harness_version"], "2.1.140");
        assert_eq!(value["tools"], json!(["Bash", "Read"]));
        assert_eq!(value["mcp_servers"][0]["name"], "tiddly");
        assert_eq!(value["mcp_servers"][0]["status"], "connected");
        assert_eq!(value["skills"], json!(["debug"]));
        assert_eq!(value["raw"]["subtype"], "init");
        let parsed: NormalizedEvent = serde_json::from_value(value).unwrap();
        assert_eq!(parsed, event);
    }

    #[test]
    fn adapter_event_lifts_to_normalized_content_chunk() {
        let adapter = AdapterEvent::ContentChunk {
            turn_id: fresh_turn_id(),
            kind: ContentKind::Text,
            text: "hi".to_owned(),
        };
        let normalized = NormalizedEvent::from(adapter);
        assert!(matches!(
            normalized,
            NormalizedEvent::ContentChunk {
                kind: ContentKind::Text,
                text,
                ..
            } if text == "hi"
        ));
    }

    #[test]
    fn adapter_event_lifts_to_normalized_turn_end_completed() {
        let adapter = AdapterEvent::TurnEnd {
            turn_id: fresh_turn_id(),
            outcome: TurnOutcome::Completed,
            ended_at: fresh_time(),
            usage: None,
        };
        let normalized = NormalizedEvent::from(adapter);
        assert!(matches!(
            normalized,
            NormalizedEvent::TurnEnd {
                outcome: TurnOutcome::Completed,
                usage: None,
                ..
            }
        ));
    }

    #[test]
    fn adapter_event_lifts_to_normalized_turn_end_failed() {
        let adapter = AdapterEvent::TurnEnd {
            turn_id: fresh_turn_id(),
            outcome: TurnOutcome::Failed {
                kind: FailureKind::AdapterFailure,
                message: "oops".to_owned(),
            },
            ended_at: fresh_time(),
            usage: None,
        };
        let normalized = NormalizedEvent::from(adapter);
        assert!(matches!(
            normalized,
            NormalizedEvent::TurnEnd {
                outcome: TurnOutcome::Failed {
                    kind: FailureKind::AdapterFailure,
                    ..
                },
                ..
            }
        ));
    }

    #[test]
    fn adapter_event_lifts_to_normalized_tool_started() {
        let adapter = AdapterEvent::ToolStarted {
            turn_id: fresh_turn_id(),
            tool_use_id: "t".to_owned(),
            kind: ToolKind::Builtin,
            name: "Bash".to_owned(),
            input: json!({}),
        };
        let normalized = NormalizedEvent::from(adapter);
        assert!(matches!(
            normalized,
            NormalizedEvent::ToolStarted { name, .. } if name == "Bash"
        ));
    }

    #[test]
    fn adapter_event_lifts_to_normalized_tool_completed() {
        let adapter = AdapterEvent::ToolCompleted {
            turn_id: fresh_turn_id(),
            tool_use_id: "t".to_owned(),
            output: "ok".to_owned(),
            is_error: false,
        };
        let normalized = NormalizedEvent::from(adapter);
        assert!(matches!(
            normalized,
            NormalizedEvent::ToolCompleted { output, .. } if output == "ok"
        ));
    }

    #[test]
    fn adapter_event_lifts_to_normalized_rate_limit_event() {
        let agent_id = fresh_agent_id();
        let adapter = AdapterEvent::RateLimitEvent {
            agent_id,
            info: json!({"x": 1}),
        };
        let normalized = NormalizedEvent::from(adapter);
        assert!(matches!(
            normalized,
            NormalizedEvent::RateLimitEvent { agent_id: a, .. } if a == agent_id
        ));
    }

    #[test]
    fn adapter_event_lifts_to_normalized_session_meta() {
        let adapter = AdapterEvent::SessionMeta {
            agent_id: fresh_agent_id(),
            model: "claude-sonnet-4-6".to_owned(),
            harness_version: "2.1.140".to_owned(),
            tools: vec![],
            mcp_servers: vec![],
            skills: vec![],
            raw: json!({}),
        };
        let normalized = NormalizedEvent::from(adapter);
        assert!(matches!(
            normalized,
            NormalizedEvent::SessionMeta { model, .. } if model == "claude-sonnet-4-6"
        ));
    }
}
