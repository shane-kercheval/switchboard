use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::harness::HarnessKind;

pub type AgentId = Uuid;

/// The identity Switchboard uses to find and resume a harness's conversation.
///
/// Modeled as a harness-shaped enum because session identity is not uniform:
/// most harnesses identify a session by one UUID, but Codex needs a string
/// thread-id **plus** the local date its rollout file is partitioned under.
/// A flat `Option<Uuid>` (the old `session_id`) couldn't represent the Codex
/// case; flat per-harness columns would allow invalid half-set states. The
/// enum makes "what identifies this agent's session" one well-typed place and
/// makes invalid states unrepresentable.
///
/// **Identity → registry, regardless of when it's learned.** A session locator
/// is agent *identity*, so it belongs on the `AgentRecord` whether it's
/// pre-generated at creation (Claude, Gemini) or assigned by the harness at
/// runtime (Codex, Antigravity). The governing rule is the *nature* of the
/// data, not its acquisition time: consolidated identity lives in the registry;
/// temporal/per-turn telemetry (cost, rate-limit) lives in a sidecar.
///
/// Wire shape (externally tagged): `{"uuid": "<uuid>"}` /
/// `{"codex": {"thread_id": "<id>", "partition_date": "YYYY-MM-DD"}}`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum SessionLocator {
    /// Claude, Gemini, Antigravity — a single session UUID, pre-generated at
    /// agent creation (Claude/Gemini) or captured at runtime (Antigravity).
    Uuid(Uuid),
    /// Codex — the runtime `thread_id` (a `String`, **not** guaranteed to be a
    /// UUID) plus the **local** date Codex partitioned its rollout file under
    /// (`~/.codex/sessions/<YYYY>/<MM>/<DD>/`). The date is captured once on the
    /// first dispatch and never recomputed — Codex appends to the original
    /// spawn-date's file even on cross-day resumes. It is a filesystem-lookup
    /// key, not a conversation timestamp; UI ordering uses event timestamps.
    Codex {
        thread_id: String,
        partition_date: NaiveDate,
    },
}

impl SessionLocator {
    /// The session UUID, if this locator is the `Uuid` variant
    /// (Claude/Gemini/Antigravity). `None` for a `Codex` locator (which has no
    /// single UUID). The natural accessor for the harnesses whose session is
    /// one UUID — used by arg-building, collision scans, and hydration.
    #[must_use]
    pub fn as_uuid(&self) -> Option<Uuid> {
        match self {
            SessionLocator::Uuid(id) => Some(*id),
            SessionLocator::Codex { .. } => None,
        }
    }

    /// The Codex `thread_id` + partition-date, if this is the `Codex` variant.
    /// `None` for a `Uuid` locator. The Codex counterpart to [`Self::as_uuid`] —
    /// used by the Codex adapter (resume + enrichment), hydration, and the
    /// collision scan.
    #[must_use]
    pub fn as_codex(&self) -> Option<(&str, NaiveDate)> {
        match self {
            SessionLocator::Codex {
                thread_id,
                partition_date,
            } => Some((thread_id, *partition_date)),
            SessionLocator::Uuid(_) => None,
        }
    }

    /// Whether this locator's shape is the one `harness` uses. The mapping is
    /// the inverse of [`crate::project::Project::register_agent`]'s per-harness
    /// assignment: `Codex` ⇒ `HarnessKind::Codex`; `Uuid` ⇒ Claude / Gemini /
    /// Antigravity. The single source of truth for "does this locator belong on
    /// this agent," so the registry update op can reject a mismatched capture
    /// rather than persist a record that would silently fail to resume.
    #[must_use]
    pub fn is_valid_for(&self, harness: HarnessKind) -> bool {
        match self {
            SessionLocator::Uuid(_) => matches!(
                harness,
                HarnessKind::ClaudeCode | HarnessKind::Gemini | HarnessKind::Antigravity
            ),
            SessionLocator::Codex { .. } => harness == HarnessKind::Codex,
        }
    }
}

/// One row in `<directory>/.switchboard/projects/<project-id>/registry.jsonl`.
///
/// Records are appended on registration. The only in-place mutation in v1 is
/// `session_locator` — set once when a runtime-assigned locator is first
/// captured (Codex/Antigravity), and on the Antigravity fork-and-heal case
/// where the conversation id changes (see [`crate::project::Project::set_session_locator`]).
/// Every other field is immutable after registration.
///
/// `project_id` is denormalized for defensive reasons — the registry path also
/// encodes the project, but carrying it in the record means a misplaced file
/// can be detected and a future cross-project read doesn't have to thread
/// directory context through every call.
///
/// `session_locator` is the agent's session identity (see [`SessionLocator`]).
/// Claude/Gemini pre-generate it at creation; Codex/Antigravity leave it `None`
/// until the harness assigns one at runtime. The field is always written (as
/// `null` when no locator yet), and the key is **required** on read — a record
/// missing it entirely is treated as corruption and fails loud (see
/// [`deserialize_required_locator`]), consistent with the Switchboard-owned
/// JSONL invariant.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentRecord {
    pub id: AgentId,
    pub project_id: Uuid,
    pub name: String,
    pub harness: HarnessKind,
    #[serde(deserialize_with = "deserialize_required_locator")]
    pub session_locator: Option<SessionLocator>,
    pub created_at: DateTime<Utc>,
}

/// Deserialize `session_locator`, requiring the key to be present (an explicit
/// `null` is allowed and yields `None`). Serde's built-in handling fills a
/// *missing* `Option` field with `None` silently; here that would mask a record
/// written before the locator migration — one carrying the old `session_id` key
/// and no `session_locator` — by loading it as "no locator" and dropping a
/// Claude/Gemini agent's resume continuity. Forcing the key present turns that
/// into a loud failure instead, surfacing an unmigrated record rather than
/// degrading silently.
fn deserialize_required_locator<'de, D>(deserializer: D) -> Result<Option<SessionLocator>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<SessionLocator>::deserialize(deserializer)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record_with_locator(locator: Option<SessionLocator>) -> AgentRecord {
        AgentRecord {
            id: Uuid::now_v7(),
            project_id: Uuid::now_v7(),
            name: "assistant".to_owned(),
            harness: HarnessKind::ClaudeCode,
            session_locator: locator,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn agent_record_roundtrips_with_uuid_locator() {
        let record = record_with_locator(Some(SessionLocator::Uuid(Uuid::now_v7())));
        let json = serde_json::to_string(&record).unwrap();
        let parsed: AgentRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, record);
    }

    #[test]
    fn agent_record_roundtrips_with_codex_locator() {
        let record = record_with_locator(Some(SessionLocator::Codex {
            thread_id: "thread-not-a-uuid-abc123".to_owned(),
            partition_date: NaiveDate::from_ymd_opt(2026, 5, 16).unwrap(),
        }));
        let json = serde_json::to_string(&record).unwrap();
        let parsed: AgentRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, record);
    }

    #[test]
    fn session_locator_wire_shapes_are_externally_tagged() {
        // Pin the on-disk shape: the M4 migration writes these exact forms.
        let uuid = Uuid::parse_str("019e2c5f-aaaa-7000-8000-000000000001").unwrap();
        let uuid_json = serde_json::to_value(SessionLocator::Uuid(uuid)).unwrap();
        assert_eq!(uuid_json["uuid"], uuid.to_string());

        let codex_json = serde_json::to_value(SessionLocator::Codex {
            thread_id: "abc".to_owned(),
            partition_date: NaiveDate::from_ymd_opt(2026, 5, 16).unwrap(),
        })
        .unwrap();
        assert_eq!(codex_json["codex"]["thread_id"], "abc");
        assert_eq!(codex_json["codex"]["partition_date"], "2026-05-16");
    }

    #[test]
    fn as_uuid_extracts_only_the_uuid_variant() {
        let id = Uuid::now_v7();
        assert_eq!(SessionLocator::Uuid(id).as_uuid(), Some(id));
        assert_eq!(
            SessionLocator::Codex {
                thread_id: "t".to_owned(),
                partition_date: NaiveDate::from_ymd_opt(2026, 5, 16).unwrap(),
            }
            .as_uuid(),
            None
        );
    }

    #[test]
    fn as_codex_extracts_only_the_codex_variant() {
        let date = NaiveDate::from_ymd_opt(2026, 5, 16).unwrap();
        assert_eq!(
            SessionLocator::Codex {
                thread_id: "t".to_owned(),
                partition_date: date,
            }
            .as_codex(),
            Some(("t", date))
        );
        assert_eq!(SessionLocator::Uuid(Uuid::now_v7()).as_codex(), None);
    }

    #[test]
    fn is_valid_for_matches_the_registration_mapping() {
        let uuid = SessionLocator::Uuid(Uuid::now_v7());
        assert!(uuid.is_valid_for(HarnessKind::ClaudeCode));
        assert!(uuid.is_valid_for(HarnessKind::Gemini));
        assert!(uuid.is_valid_for(HarnessKind::Antigravity));
        assert!(!uuid.is_valid_for(HarnessKind::Codex));

        let codex = SessionLocator::Codex {
            thread_id: "t".to_owned(),
            partition_date: NaiveDate::from_ymd_opt(2026, 5, 16).unwrap(),
        };
        assert!(codex.is_valid_for(HarnessKind::Codex));
        assert!(!codex.is_valid_for(HarnessKind::ClaudeCode));
        assert!(!codex.is_valid_for(HarnessKind::Gemini));
        assert!(!codex.is_valid_for(HarnessKind::Antigravity));
    }

    #[test]
    fn session_locator_serializes_as_null_when_none() {
        // Codex/Antigravity agents start with `session_locator: None` — the wire
        // shape must emit null rather than omitting the field, so a consumer can
        // tell "no locator yet" from a truncated record.
        let record = record_with_locator(None);
        let json = serde_json::to_string(&record).unwrap();
        assert!(json.contains("\"session_locator\":null"), "got: {json}");
        let parsed: AgentRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.session_locator, None);
    }

    #[test]
    fn record_missing_session_locator_fails_to_deserialize() {
        // Post-migration contract: the transitional `#[serde(default)]` shim is
        // gone, so a record lacking the `session_locator` key is corruption and
        // must fail loud rather than silently loading as `None` (which would
        // mask session-identity loss). Every migrated record carries the field
        // explicitly, written as `null` when absent.
        let json = r#"{"id":"019e2c5f-aaaa-7000-8000-000000000001","project_id":"019e2c5f-bbbb-7000-8000-000000000002","name":"legacy","harness":"claude_code","created_at":"2026-05-15T12:30:45Z"}"#;
        let err = serde_json::from_str::<AgentRecord>(json)
            .expect_err("a record without session_locator must fail to deserialize");
        assert!(
            err.to_string().contains("session_locator"),
            "error should name the missing field, got: {err}"
        );
    }
}
