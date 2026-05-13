use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// UUID v7 turn identifier — consistent with `AgentId` and `ProjectId`.
pub type TurnId = Uuid;

/// Events emitted by harness adapters. `TurnStart` is deliberately absent — it is
/// dispatcher-owned (M1.4) and synthesized before the stream is established. Excluding
/// it here makes the invariant type-enforced: no adapter author can accidentally emit it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum AdapterEvent {
    ContentChunk {
        turn_id: TurnId,
        text: String,
    },
    TurnEnd {
        turn_id: TurnId,
        outcome: TurnOutcome,
        ended_at: DateTime<Utc>,
    },
}

/// Wire format across the IPC boundary to the frontend. The dispatcher constructs
/// `TurnStart` at dispatch time; adapter events lift into the remaining variants
/// via `From<AdapterEvent>`.
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
        text: String,
    },
    TurnEnd {
        turn_id: TurnId,
        outcome: TurnOutcome,
        ended_at: DateTime<Utc>,
    },
}

impl From<AdapterEvent> for NormalizedEvent {
    fn from(e: AdapterEvent) -> Self {
        match e {
            AdapterEvent::ContentChunk { turn_id, text } => {
                NormalizedEvent::ContentChunk { turn_id, text }
            }
            AdapterEvent::TurnEnd {
                turn_id,
                outcome,
                ended_at,
            } => NormalizedEvent::TurnEnd {
                turn_id,
                outcome,
                ended_at,
            },
        }
    }
}

/// Outcome of a completed turn. The `kind` field on `Failed` is load-bearing for
/// M5's partial-failure policy: `HarnessError` (model/API issue) vs `AdapterFailure`
/// (subprocess crash, parse error, infrastructure) have different retry semantics.
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed_turn_id() -> TurnId {
        Uuid::now_v7()
    }

    fn fixed_time() -> DateTime<Utc> {
        Utc::now()
    }

    #[test]
    fn turn_start_wire_shape() {
        let turn_id = fixed_turn_id();
        let started_at = fixed_time();
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
    fn content_chunk_wire_shape() {
        let turn_id = fixed_turn_id();
        let event = NormalizedEvent::ContentChunk {
            turn_id,
            text: "hello".to_owned(),
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], "content_chunk");
        assert_eq!(value["text"], "hello");
        let parsed: NormalizedEvent = serde_json::from_value(value).unwrap();
        assert_eq!(parsed, event);
    }

    #[test]
    fn turn_end_completed_wire_shape() {
        let turn_id = fixed_turn_id();
        let event = NormalizedEvent::TurnEnd {
            turn_id,
            outcome: TurnOutcome::Completed,
            ended_at: fixed_time(),
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], "turn_end");
        assert_eq!(value["outcome"]["status"], "completed");
        let parsed: NormalizedEvent = serde_json::from_value(value).unwrap();
        assert_eq!(parsed, event);
    }

    #[test]
    fn turn_end_failed_wire_shape() {
        let turn_id = fixed_turn_id();
        let event = NormalizedEvent::TurnEnd {
            turn_id,
            outcome: TurnOutcome::Failed {
                kind: FailureKind::HarnessError,
                message: "bad model".to_owned(),
            },
            ended_at: fixed_time(),
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], "turn_end");
        assert_eq!(value["outcome"]["status"], "failed");
        assert_eq!(value["outcome"]["kind"], "harness_error");
        assert_eq!(value["outcome"]["message"], "bad model");
        let parsed: NormalizedEvent = serde_json::from_value(value).unwrap();
        assert_eq!(parsed, event);
    }

    #[test]
    fn adapter_failure_kind_wire_shape() {
        let turn_id = fixed_turn_id();
        let event = NormalizedEvent::TurnEnd {
            turn_id,
            outcome: TurnOutcome::Failed {
                kind: FailureKind::AdapterFailure,
                message: "crash".to_owned(),
            },
            ended_at: fixed_time(),
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["outcome"]["kind"], "adapter_failure");
    }

    #[test]
    fn adapter_event_lifts_to_normalized_content_chunk() {
        let turn_id = fixed_turn_id();
        let adapter = AdapterEvent::ContentChunk {
            turn_id,
            text: "hi".to_owned(),
        };
        let normalized = NormalizedEvent::from(adapter);
        assert!(matches!(
            normalized,
            NormalizedEvent::ContentChunk { text, .. } if text == "hi"
        ));
    }

    #[test]
    fn adapter_event_lifts_to_normalized_turn_end_completed() {
        let turn_id = fixed_turn_id();
        let adapter = AdapterEvent::TurnEnd {
            turn_id,
            outcome: TurnOutcome::Completed,
            ended_at: fixed_time(),
        };
        let normalized = NormalizedEvent::from(adapter);
        assert!(matches!(
            normalized,
            NormalizedEvent::TurnEnd {
                outcome: TurnOutcome::Completed,
                ..
            }
        ));
    }

    #[test]
    fn adapter_event_lifts_to_normalized_turn_end_failed() {
        let turn_id = fixed_turn_id();
        let adapter = AdapterEvent::TurnEnd {
            turn_id,
            outcome: TurnOutcome::Failed {
                kind: FailureKind::AdapterFailure,
                message: "oops".to_owned(),
            },
            ended_at: fixed_time(),
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
}
