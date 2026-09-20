//! User-global harness quota snapshots (`usage.yaml`) — the newest usage
//! reading seen for each harness, from any agent in any project.
//!
//! **Why this is user-global rather than per-project or per-agent.** A quota
//! belongs to the *account* the harness is logged into, not to an agent and not
//! to a project: one `claude login` / `codex login` per machine, and every turn
//! any agent runs reports the same windows. Storing a reading per agent inside a
//! project published one fact many times at many staleness levels, which is
//! what made sibling cards disagree and made reopening an old project restore
//! that project's older reading. One entry per harness, in one user-global file,
//! removes the whole class.
//!
//! **Deliberately opaque, and deliberately without a merge rule.** The stored
//! value is whatever the frontend put there, carried as raw JSON exactly as
//! [`crate::commands::AgentConversationMeta::last_rate_limit`] is. The rule for
//! which of several readings is newest lives in **one** place, in the frontend
//! beside the code that renders it, rather than being written once here and
//! again there; this module's entire job is durability. That is the same
//! division as [`crate::workspace`], which persists view-state it does not
//! interpret.
//!
//! A missing or corrupt `usage.yaml` degrades to empty. Nothing here is
//! load-bearing: the cost is an empty usage section until the next turn reports
//! a reading, never a failed load, and every window in a reading carries its own
//! absolute reset time so a stale entry expires itself rather than needing to be
//! aged out here.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use switchboard_core::CoreError;

use crate::error::AppError;

/// Every harness's newest reading, exactly as `usage.yaml` persists it.
///
/// Keyed by the harness's wire name as a **plain `String`** rather than
/// `HarnessKind`, and valued as raw JSON: a file written by a build that knows
/// a harness this one does not must still round-trip instead of failing to
/// deserialize and taking every other harness's reading down with it. The
/// frontend already validates the key before it renders anything.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct HarnessUsage {
    /// `BTreeMap` so the file is deterministically ordered, and `#[serde(default)]`
    /// so a file predating the field loads as empty rather than erroring.
    ///
    /// **Public, and with no methods over it**, unlike the sibling registries:
    /// those own behaviour worth encapsulating, while this type is pure transport
    /// between the frontend that owns the map and the file that holds it. The
    /// frontend replaces it whole, so there is nothing for a setter to guard.
    #[serde(default)]
    pub harnesses: BTreeMap<String, serde_json::Value>,
}

/// Outcome of reading the file: the value to use this session plus whether
/// persisting *over the file we read* is safe.
pub struct LoadOutcome {
    pub usage: HarnessUsage,
    /// `false` only when the file exists but the **read itself** failed: it may
    /// hold real readings we could not parse, so this session must not overwrite
    /// it. A missing file and a corrupt-YAML file are both `true` — neither has
    /// anything recoverable to clobber. Same three-case contract as
    /// [`crate::git_registry::load`].
    pub persistable: bool,
}

/// Read the snapshots from `path`. Never fails; see [`LoadOutcome::persistable`]
/// for the unreadable-versus-corrupt distinction.
pub fn load(path: &Path) -> LoadOutcome {
    if !path.exists() {
        return LoadOutcome {
            usage: HarnessUsage::default(),
            persistable: true,
        };
    }
    match switchboard_core::read_yaml::<HarnessUsage>(path) {
        Ok(usage) => LoadOutcome {
            usage,
            persistable: true,
        },
        Err(e @ CoreError::CorruptYaml { .. }) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "usage.yaml is corrupt — starting with no quota snapshots; the next reading will replace it"
            );
            LoadOutcome {
                usage: HarnessUsage::default(),
                persistable: true,
            }
        }
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "usage.yaml could not be read — persistence disabled this session to avoid overwriting it"
            );
            LoadOutcome {
                usage: HarnessUsage::default(),
                persistable: false,
            }
        }
    }
}

/// Persist the snapshots to `path`, creating the parent directory if needed.
/// Atomic temp-write plus rename via `switchboard_core::write_yaml`.
pub fn save(path: &Path, usage: &HarnessUsage) -> Result<(), AppError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| AppError::HarnessUsagePersist {
            path: path.to_owned(),
            source,
        })?;
    }
    switchboard_core::write_yaml(path, usage)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    fn reading(percent: f64) -> serde_json::Value {
        serde_json::json!({
            "payload": { "primary": { "used_percent": percent } },
            "observed_at": "2026-09-18T20:30:00Z",
        })
    }

    #[test]
    fn absent_file_loads_empty_and_stays_persistable() {
        let dir = tempdir().unwrap();
        let outcome = load(&dir.path().join("usage.yaml"));
        assert!(outcome.usage.harnesses.is_empty());
        assert!(
            outcome.persistable,
            "a fresh install has nothing to clobber"
        );
    }

    #[test]
    fn readings_round_trip_through_the_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("usage.yaml");
        let usage = HarnessUsage {
            harnesses: BTreeMap::from([
                ("codex".to_owned(), reading(93.0)),
                ("claude_code".to_owned(), reading(71.0)),
            ]),
        };
        save(&path, &usage).unwrap();

        let loaded = load(&path).usage;
        assert_eq!(loaded, usage);
        // The opaque payload survives verbatim — the whole point of carrying it
        // as raw JSON rather than a typed shape this crate would have to track.
        assert_eq!(
            loaded.harnesses["codex"].pointer("/payload/primary/used_percent"),
            Some(&serde_json::json!(93.0))
        );
    }

    #[test]
    fn a_per_window_reading_round_trips_with_every_nested_field() {
        // Claude's windows are retained individually, each carrying the context of
        // the reading that delivered it. That is a map inside a map inside the
        // entry, and it reaches the file through the same untyped passthrough as a
        // flat reading — so nothing here needs to know the shape, but something
        // has to prove the nesting survives a YAML round trip.
        let dir = tempdir().unwrap();
        let path = dir.path().join("usage.yaml");
        let entry = serde_json::json!({
            "payload": { "status": "allowed", "unifiedWindows": {} },
            "observed_at": "2026-09-18T21:00:00Z",
            "windows": {
                "seven_day_overage_included": {
                    "window": { "utilization": 1.0, "resetsAt": 1_789_845_487u64 },
                    "status": "rejected",
                    "rate_limit_type": "seven_day_overage_included",
                    "is_using_overage": false,
                    "observed_at": "2026-09-18T20:00:00Z",
                    "model": "claude-fable-5-1",
                    "agent_id": "agent-1"
                }
            }
        });
        let usage = HarnessUsage {
            harnesses: BTreeMap::from([("claude_code".to_owned(), entry.clone())]),
        };
        save(&path, &usage).unwrap();

        let loaded = load(&path).usage;
        assert_eq!(loaded.harnesses["claude_code"], entry);
    }

    #[test]
    fn an_unknown_harness_key_round_trips_instead_of_failing_the_file() {
        // Forward compatibility is the reason the keys are strings: a reading
        // written by a build that knows a harness this one does not must not
        // take the harnesses it *does* know down with it.
        let dir = tempdir().unwrap();
        let path = dir.path().join("usage.yaml");
        std::fs::write(
            &path,
            "harnesses:\n  codex:\n    used: 93\n  some_future_harness:\n    used: 12\n",
        )
        .unwrap();

        let loaded = load(&path).usage;
        assert_eq!(loaded.harnesses.len(), 2);
        assert!(loaded.harnesses.contains_key("some_future_harness"));
    }

    #[test]
    fn a_corrupt_file_degrades_to_empty_and_may_be_replaced() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("usage.yaml");
        std::fs::write(&path, "harnesses: [this is not a map]\n").unwrap();

        let outcome = load(&path);
        assert!(outcome.usage.harnesses.is_empty());
        assert!(
            outcome.persistable,
            "unparseable YAML holds nothing worth preserving, so the next reading may replace it"
        );
    }
}
