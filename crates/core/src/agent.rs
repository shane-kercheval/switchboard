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
/// Records are appended on registration. Four things mutate after that, each
/// through its own dedicated `Project` method (never a generic update API):
/// `name` (rename), `model` / `effort` (selection changes), `session_locator`
/// (runtime capture for Codex/Antigravity, including the Antigravity
/// fork-and-heal case — see [`crate::project::Project::set_session_locator`]),
/// and the records' physical order (user reordering — file order *is* the
/// roster's display order). `id`, `project_id`, `harness`, and `created_at`
/// are immutable.
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
    /// The user-selected model this agent runs on, sent to the harness on every
    /// dispatch when set (omitted when `None`, so the harness uses its default).
    /// Free-text, not validated against an enum: no harness exposes a queryable
    /// model list and some values are plan-gated, so a bad value surfaces as a
    /// failed turn rather than a registration error. Only harnesses where
    /// [`HarnessKind::supports_model_selection`] holds ever carry a value.
    ///
    /// Plain `Option` with no serde attribute: serde fills a missing field of
    /// `Option` type with `None`, so a record written before this field existed
    /// deserializes to `None` — the correct backward-compatible default.
    /// (Deliberately unlike `session_locator`, which adds a custom deserializer
    /// precisely to *defeat* that permissive default and fail loud on absence.)
    pub model: Option<String>,
    /// The user-selected reasoning-effort level, sent on every dispatch when set
    /// (omitted when `None`). A closed per-harness enum at the UI boundary, but
    /// stored as a `String` to keep this field harness-agnostic. Only harnesses
    /// where [`HarnessKind::supports_effort_selection`] holds ever carry a
    /// value. Same backward-compat rationale as `model`.
    pub effort: Option<String>,
    /// For a forked agent: the **parent session UUID** to `--resume` from when
    /// this agent's own session file does not exist yet. `None` for every agent
    /// that wasn't created by forking.
    ///
    /// Stores the parent's *session* id, not the parent's *agent* id, so the
    /// record is self-contained: the fork still materializes after the parent
    /// agent is deleted (Switchboard never deletes harness session files).
    ///
    /// **Permanent, and inert after first use.** It is never cleared once the
    /// fork materializes — whether a dispatch forks or plainly resumes is
    /// derived from the agent's own session file existing, not from consuming
    /// this field (see `claude_code::build_args`). Keeping it makes a first
    /// dispatch that died before creating the file retry the fork
    /// automatically.
    ///
    /// **This field does two jobs, and they only coincide for a deferred
    /// fork.** (1) *Operational:* the materialization token — the session to
    /// resume from until this agent has one of its own. (2) *Display:* durable
    /// lineage — "this agent is a branch of that session" — which is why it
    /// outlives job (1) rather than being cleared. Both are served by one UUID
    /// because Claude's fork is deferred and caller-assigned (see
    /// [`HarnessKind::supports_session_fork`]). A harness whose fork is *eager*
    /// would need job (2) and not job (1), and its lineage identity would not
    /// be a Claude session UUID — so it needs its own sibling field, **not** a
    /// widening or overloading of this one.
    ///
    /// Only harnesses where [`HarnessKind::supports_session_fork`] holds ever
    /// carry `Some` — enforced when writing, at the registration chokepoint
    /// (`Project::register_agent_inner`), and re-checked when reading by
    /// [`AgentRecord::validate`], which `Project::list_agents` runs over every
    /// record. Serde alone cannot do this: it validates each field in isolation
    /// and cannot compare one against `harness`.
    ///
    /// So: the Codex/Gemini/Antigravity **adapters** ignore this field, and are
    /// correct to — their `build_args` never reads it. Do not add "defensive"
    /// handling there; it would imply the state is reachable through normal use
    /// and invert the invariant. But the field is **not inert for non-Claude
    /// agents**: the harness-agnostic materializing-fork gates in the app layer
    /// (`ensure_materializing_fork_may_dispatch`, and the workflow preflight)
    /// read it for every agent regardless of harness, so a corrupted record
    /// would send a Codex agent down the unmaterialized-branch path and could
    /// block its sends behind an unrelated "parent". That is the reachable
    /// consequence to reason about, not adapter behavior.
    ///
    /// Same plain-`Option` backward-compat rationale as `model` / `effort`
    /// above: a record written before forking existed legitimately lacks the key
    /// and must load as `None`.
    pub forked_from_session: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

impl AgentRecord {
    /// Re-check, on **read**, the cross-field invariants the registration
    /// chokepoint enforces on write.
    ///
    /// Serde validates one field at a time and cannot compare a field against
    /// `harness`, so a hand-edited or corrupted `registry.jsonl` can produce
    /// records the writer would have refused: a Codex agent carrying fork
    /// provenance, a locator of the wrong shape for its harness, a model on a
    /// harness with no model axis. None of those are inert — the harness-agnostic
    /// dispatch gates read fork provenance for every agent regardless of harness,
    /// and would treat such a record as a branch waiting to materialize.
    ///
    /// **Fails the whole load**, matching how this file already treats a corrupt
    /// line (`CoreError::CorruptJsonl`) and how `session_locator`'s own
    /// deserializer treats a missing key: a registry that contradicts itself is
    /// not partially usable, and silently dropping the offending agent would look
    /// to the user exactly like the data loss we are trying to make visible.
    pub fn validate(&self) -> crate::error::Result<()> {
        use crate::error::CoreError;
        if let Some(locator) = &self.session_locator
            && !locator.is_valid_for(self.harness)
        {
            return Err(CoreError::SessionLocatorHarnessMismatch {
                agent_id: self.id,
                harness: self.harness,
            });
        }
        if self.forked_from_session.is_some() && !self.harness.supports_session_fork() {
            return Err(CoreError::SessionForkUnsupported {
                harness: self.harness,
            });
        }
        if self.model.is_some() && !self.harness.supports_model_selection() {
            return Err(CoreError::SelectionUnsupported {
                harness: self.harness,
                axis: crate::harness::SelectionAxis::Model,
            });
        }
        if self.effort.is_some() && !self.harness.supports_effort_selection() {
            return Err(CoreError::SelectionUnsupported {
                harness: self.harness,
                axis: crate::harness::SelectionAxis::Effort,
            });
        }
        Ok(())
    }
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

/// Normalize a free-text selection (`model` / `effort`): trim surrounding
/// whitespace and treat empty/whitespace-only as "unset" (`None`).
///
/// Applied at **both** boundaries it can be written through — the IPC command
/// (friendly, early) and the core persistence methods (airtight, regardless of
/// caller) — so the registry never stores a blank selection, which would
/// dispatch `--model ""` (or `-c model_reasoning_effort=`) on every turn and
/// fail with a non-obvious cause. Sharing one definition keeps that
/// dispatch-safety guard from drifting between the two layers.
///
/// This is footgun-normalization, not value validation: a non-blank value is
/// never judged "valid" here (a bad model surfaces as a failed turn, by design).
#[must_use]
pub fn normalize_selection(value: Option<String>) -> Option<String> {
    value.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty())
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
            model: None,
            effort: None,
            forked_from_session: None,
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
    fn agent_record_roundtrips_with_model_and_effort() {
        let mut record = record_with_locator(Some(SessionLocator::Uuid(Uuid::now_v7())));
        record.model = Some("sonnet".to_owned());
        record.effort = Some("high".to_owned());
        let json = serde_json::to_string(&record).unwrap();
        let parsed: AgentRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, record);
        assert_eq!(parsed.model.as_deref(), Some("sonnet"));
        assert_eq!(parsed.effort.as_deref(), Some("high"));
    }

    #[test]
    fn agent_record_serializes_unset_model_and_effort_as_null() {
        // Unset fields persist as explicit `null` (plain `Option`, no
        // `skip_serializing_if`), so the on-disk record is self-describing.
        let record = record_with_locator(Some(SessionLocator::Uuid(Uuid::now_v7())));
        let json = serde_json::to_string(&record).unwrap();
        assert!(json.contains("\"model\":null"), "got: {json}");
        assert!(json.contains("\"effort\":null"), "got: {json}");
    }

    #[test]
    fn record_missing_model_and_effort_deserializes_as_none() {
        // Backward-compat safeguard: a record written before these fields
        // existed has neither key. Unlike `session_locator` (which fails loud on
        // absence via a custom deserializer), a plain `Option` field is filled
        // with `None` when missing — the correct default for selections that
        // simply weren't a concept yet.
        let json = r#"{"id":"019e2c5f-aaaa-7000-8000-000000000001","project_id":"019e2c5f-bbbb-7000-8000-000000000002","name":"legacy","harness":"claude_code","session_locator":null,"created_at":"2026-05-15T12:30:45Z"}"#;
        let parsed: AgentRecord =
            serde_json::from_str(json).expect("missing model/effort must default to None");
        assert_eq!(parsed.model, None);
        assert_eq!(parsed.effort, None);
        assert_eq!(parsed.session_locator, None);
    }

    #[test]
    fn agent_record_roundtrips_with_fork_provenance() {
        let parent = Uuid::now_v7();
        let mut record = record_with_locator(Some(SessionLocator::Uuid(Uuid::now_v7())));
        record.forked_from_session = Some(parent);
        let json = serde_json::to_string(&record).unwrap();
        let parsed: AgentRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, record);
        assert_eq!(parsed.forked_from_session, Some(parent));
    }

    #[test]
    fn agent_record_serializes_unset_fork_provenance_as_null() {
        // Self-describing on disk, like `model` / `effort`: a non-forked agent
        // records "not a fork" explicitly rather than by omission.
        let record = record_with_locator(Some(SessionLocator::Uuid(Uuid::now_v7())));
        let json = serde_json::to_string(&record).unwrap();
        assert!(json.contains("\"forked_from_session\":null"), "got: {json}");
    }

    #[test]
    fn record_missing_fork_provenance_deserializes_as_none() {
        // Backward compat: every record written before forking existed lacks
        // this key. Unlike `session_locator` (fail-loud on absence), a missing
        // plain `Option` must default to `None` — "this agent is not a fork" is
        // the correct reading of a pre-fork record, not corruption.
        let json = r#"{"id":"019e2c5f-aaaa-7000-8000-000000000001","project_id":"019e2c5f-bbbb-7000-8000-000000000002","name":"legacy","harness":"claude_code","session_locator":null,"model":null,"effort":null,"created_at":"2026-05-15T12:30:45Z"}"#;
        let parsed: AgentRecord =
            serde_json::from_str(json).expect("missing forked_from_session must default to None");
        assert_eq!(parsed.forked_from_session, None);
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
        // Pin the on-disk shape: the migration writes these exact forms.
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
