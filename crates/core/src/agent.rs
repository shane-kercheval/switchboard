use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::harness::HarnessKind;

pub type AgentId = Uuid;

/// One row in `<directory>/.switchboard/projects/<project-id>/registry.jsonl`.
///
/// Append-only. Once written, an `AgentRecord` is never modified or deleted in v1.
///
/// `project_id` is denormalized for defensive reasons — the registry path also
/// encodes the project, but carrying it in the record means a misplaced file
/// can be detected and a future cross-project read doesn't have to thread
/// directory context through every call.
///
/// `session_id` is pre-generated at agent creation time for Claude Code agents
/// (passed to `claude` via `--session-id <uuid>` in M1.3). For future Codex
/// agents it stays `None` — Codex assigns its own session ID from the stream
/// and stores it in a per-agent sidecar.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentRecord {
    pub id: AgentId,
    pub project_id: Uuid,
    pub name: String,
    pub harness: HarnessKind,
    pub session_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_record_roundtrips_through_json() {
        let record = AgentRecord {
            id: Uuid::now_v7(),
            project_id: Uuid::now_v7(),
            name: "assistant".to_owned(),
            harness: HarnessKind::ClaudeCode,
            session_id: Some(Uuid::now_v7()),
            created_at: Utc::now(),
        };
        let json = serde_json::to_string(&record).unwrap();
        let parsed: AgentRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, record);
    }

    #[test]
    fn session_id_serializes_as_null_when_none() {
        // Codex (M2+) will land with session_id: None — make sure the wire shape
        // emits null rather than omitting the field (which would happen with
        // #[serde(skip_serializing_if = "Option::is_none")]). This test guards
        // against accidentally adding that attribute.
        let record = AgentRecord {
            id: Uuid::now_v7(),
            project_id: Uuid::now_v7(),
            name: "codex-agent".to_owned(),
            harness: HarnessKind::ClaudeCode,
            session_id: None,
            created_at: Utc::now(),
        };
        let json = serde_json::to_string(&record).unwrap();
        assert!(json.contains("\"session_id\":null"), "got: {json}");
        let parsed: AgentRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.session_id, None);
    }
}
