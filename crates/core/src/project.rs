use std::fs::create_dir_all;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::agent::{
    AgentId, AgentProfile, AgentProfileSlot, AgentProfiles, AgentRecord, SessionLocator,
    normalize_selection,
};
use crate::error::{CoreError, Result};
use crate::harness::{HarnessKind, SelectionAxis};
use crate::ids::{DirectoryId, ProjectId};
use crate::io::{append_jsonl, read_jsonl, read_yaml, write_jsonl, write_yaml};
use crate::name::{canonicalize_for_uniqueness, validate_name};
use crate::paths::{
    ATTACHMENTS_DIR, CONFIG_FILE, JOURNAL_FILE, PINS_FILE, REGISTRY_FILE, RUNS_DIR,
};

/// `pub(crate)` so `Directory::rename_project` can stamp the current version
/// when rewriting `config.yaml` without a redundant read-back.
pub(crate) const PROJECT_CONFIG_VERSION: u32 = 1;

/// One entry in `<directory>/.switchboard/projects.jsonl` — the directory-level
/// index of which projects exist under this directory. Appended on
/// `create_project`; rewritten in place on rename/delete (see
/// `Directory::rename_project` / `Directory::delete_project`), exactly as
/// `registry.jsonl` is for agents.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectSummary {
    pub id: ProjectId,
    pub name: String,
    pub created_at: DateTime<Utc>,
}

/// On-disk shape of a project's `config.yaml`.
///
/// **Authority is per field, not per file.** Stating it once for the whole
/// struct is what produced a field documented by analogy to a neighbour whose
/// contract ran the other way, so each field below says which copy wins and
/// why. Note the id is *not* among them: it is the enclosing directory's name
/// (`projects/<id>/`), never written into this file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectConfig {
    pub version: u32,
    /// **Config is canonical; the index copy is what renders.** Not a
    /// contradiction — they answer different questions. `rename_project` writes
    /// this file first and the index second as the commit, so after a partial
    /// failure this is ahead and the index is behind; correctness and recovery
    /// take this one, while anything the user reads (the project list, a
    /// collision error naming another project) quotes the index, because that is
    /// what they are looking at. Quoting this file in a user-facing string would
    /// name a project by a name nothing on screen shows.
    pub name: String,
    /// Same contract as `name`: canonical here, denormalized into the index
    /// entry, and never independently mutated — only `create_on_disk` writes it,
    /// and `rename_project` carries the index entry's value back unchanged.
    pub created_at: DateTime<Utc>,
    /// The working directory this project belongs to — **a recovery record,
    /// never read at runtime.**
    ///
    /// **`projects.jsonl` is authoritative; this copy is never read at
    /// runtime.** Deliberately stated on its own rather than by analogy to
    /// `name`, whose contract runs the other way. Nothing resolves a dispatch
    /// cwd from here: [`load`] must not populate [`Project::directory`] from it,
    /// because doing so would bypass the catalog — the only place that can
    /// detect a duplicated or missing directory id — in favour of a copy with no
    /// such checks.
    ///
    /// It exists so the project tree is self-describing. Without it, losing
    /// `projects.jsonl` and `directories.jsonl` together leaves every project's
    /// data intact with no record of which directory any of it belongs to. With
    /// it, a repair tool can rebuild the index and see which projects share a
    /// directory identity. It does **not** recover the catalog's
    /// `directory_id -> path` mapping, which still needs migration records or
    /// the user re-pointing each id.
    ///
    /// `None` means the project predates the user-global store (the legacy
    /// `<directory>/.switchboard/` layout, whose owning directory was implied by
    /// the path). Every project created in or migrated into the store carries
    /// `Some`. **Any future writer that can change a project's owning directory
    /// must update this alongside the index** — today the only writers are
    /// creation and rename, and both stamp it from the index entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directory_id: Option<DirectoryId>,
}

/// The per-caller inputs to [`Project::register_agent_inner`].
///
/// A struct rather than a positional list because the fields it carries include
/// two adjacent `Option<String>`s (`model`, `effort`) that a positional call
/// could transpose silently — the compiler cannot tell them apart, and a
/// transposed pair persists a wrong-but-valid record. Naming them at each call
/// site makes that class of mistake unrepresentable.
///
/// Deliberately **not** validating: see the note on `register_agent_inner`.
struct NewAgent<'a> {
    name: &'a str,
    harness: HarnessKind,
    session_locator: Option<SessionLocator>,
    model: Option<String>,
    effort: Option<String>,
    profiles: AgentProfiles,
    forked_from_session: Option<Uuid>,
    forked_from_session_home: Option<PathBuf>,
}

/// A task-scoped project within a working directory. Holds agents in its registry.
#[derive(Debug, Clone)]
pub struct Project {
    pub id: ProjectId,
    pub directory: PathBuf,
    pub config: ProjectConfig,
    pub root: PathBuf,
    pub registry_path: PathBuf,
}

impl Project {
    /// User-facing project name, sourced from `config.yaml` (the canonical
    /// record; the `projects.jsonl` summary's `name` is a denormalized copy).
    pub fn name(&self) -> &str {
        &self.config.name
    }

    /// Path to this project's conversation journal (`journal.jsonl`) — the
    /// Switchboard-owned record of user sends + non-completed-turn outcomes
    /// (see [`crate::journal`]). Runtime data; `.gitignore`d like the rest of
    /// `projects/`.
    pub fn journal_path(&self) -> PathBuf {
        self.root.join(JOURNAL_FILE)
    }

    /// Path to this project's mutable message-pin list. Pins contain only
    /// stable message identifiers and timestamps, never message content.
    pub fn pins_path(&self) -> PathBuf {
        self.root.join(PINS_FILE)
    }

    /// Directory holding this project's staged attachment files
    /// (`projects/<id>/attachments/`), and the current staging target.
    ///
    /// Attachments are handed to agents as absolute paths in the prompt footer
    /// (see [`crate::render_prompt_with_attachments`]), which is what every
    /// harness can read regardless of sandbox — the location itself carries no
    /// requirement.
    ///
    /// **Per-project, and staying that way.** A store-wide `attachments/` was
    /// designed and reversed: it would have turned project delete from a plain
    /// directory removal into an all-projects reference sweep, moving a GC bug's
    /// blast radius from one project to every project. Keeping them here means
    /// delete reclaims them by removing the project root. Do not reintroduce a
    /// store-level equivalent.
    pub fn attachments_dir(&self) -> PathBuf {
        self.root.join(ATTACHMENTS_DIR)
    }

    /// Directory holding this project's workflow-run records
    /// (`projects/<id>/runs/`). One `<run-id>.jsonl` per run, written by the
    /// workflow interpreter as progress/terminal bookkeeping (no agent content —
    /// resume is deferred, so these are surfacing/abandon records, not replay
    /// state). Runtime data; `.gitignore`d like the rest of `projects/`. Created
    /// lazily on the first run.
    pub fn runs_dir(&self) -> PathBuf {
        self.root.join(RUNS_DIR)
    }

    /// Path to a specific run's record file (`projects/<id>/runs/<run-id>.jsonl`).
    pub fn run_path(&self, run_id: Uuid) -> PathBuf {
        self.runs_dir().join(format!("{run_id}.jsonl"))
    }

    /// Append a new agent to this project's registry. Validates the name (regex +
    /// per-project uniqueness with hyphen↔underscore + case normalization), generates
    /// a UUID v7 `AgentId`, and (for Claude Code) pre-generates a UUID v7
    /// `SessionLocator::Uuid` the adapter will pass via `--session-id <uuid>`.
    ///
    /// # Concurrency
    ///
    /// Not safe to call concurrently against the *same `Project` instance* — the
    /// read-check-then-append sequence has a TOCTOU window. Callers must
    /// serialize access (the dispatcher / `AppState` mutex does this).
    /// Concurrent calls against *different* `Project` instances (in the same
    /// or different directories) are fine; cross-process serialization within
    /// one directory is future work.
    ///
    /// # Durability
    ///
    /// On the rare path where `append_jsonl` reports a post-write durability
    /// (fsync) failure, this returns `Err` even though the record may already
    /// be on disk (`append_jsonl` syncs after writing). The caller must not
    /// treat that as "nothing happened": a subsequent retry can hit
    /// `DuplicateAgentName` because the record is visible, and the agent will
    /// appear on the next `list_agents`. There is no destructive cleanup to
    /// undo here (unlike `Directory::create_project`), so no rollback applies.
    ///
    /// `model` / `effort` are the user-selected per-agent settings (`None` =
    /// harness default). Every supported harness drives both axes today, so the
    /// capability gates in `register_agent_inner` reject nothing at present; they
    /// remain as the forcing function for a harness that lacks an axis (see the
    /// note at those gates). Antigravity does constrain the *pair* — an effort
    /// with no model is rejected, since its valid levels are per-model.
    /// This generic create path can't express that in its signature the way the
    /// attach variants do, so it relies on that shared chokepoint. The commands layer also validates
    /// first to return a friendlier error, but `core` is the backstop that
    /// keeps an inapplicable selection out of the registry regardless of
    /// caller.
    pub fn register_agent(
        &self,
        name: &str,
        harness: HarnessKind,
        model: Option<String>,
        effort: Option<String>,
    ) -> Result<AgentRecord> {
        self.register_agent_with_profiles(name, harness, model, effort, None)
    }

    /// Register a fresh agent with an optional secondary execution profile.
    /// New agents always begin on Primary; switching is a separate explicit
    /// action after registration.
    pub fn register_agent_with_profiles(
        &self,
        name: &str,
        harness: HarnessKind,
        model: Option<String>,
        effort: Option<String>,
        secondary: Option<AgentProfile>,
    ) -> Result<AgentRecord> {
        // Harness-asymmetry rule (which harnesses can pre-generate their
        // session locator at registration vs. learn it at runtime):
        // - Claude Code pre-generates a UUID v7 locator; passed via
        //   `--session-id`/`--resume`.
        // - Codex and Antigravity leave it `None`: their session id is
        //   assigned by the harness at runtime (Codex's `thread_id` from
        //   `thread.started`; Antigravity's server-assigned conversation
        //   UUID), so it isn't knowable at registration time. The adapter
        //   captures it on first dispatch and it's persisted to this record's
        //   `session_locator` via `set_session_locator`.
        let session_locator = match harness {
            HarnessKind::ClaudeCode => Some(SessionLocator::Uuid(Uuid::now_v7())),
            HarnessKind::Codex | HarnessKind::Antigravity => None,
        };
        self.register_agent_inner(NewAgent {
            name,
            harness,
            session_locator,
            model,
            effort,
            profiles: AgentProfiles {
                secondary,
                active: AgentProfileSlot::Primary,
            },
            forked_from_session: None,
            forked_from_session_home: None,
        })
    }

    /// Branch an existing agent's conversation into a new agent.
    ///
    /// The fork is an ordinary, equally first-class agent: it pre-generates its
    /// own UUID v7 session locator (exactly like [`Self::register_agent`]'s
    /// Claude arm, because Claude lets the caller choose a forked session's id)
    /// and inherits the source's `model` / `effort` selections. What makes it a
    /// fork is only [`AgentRecord::forked_from_session`], carrying the source's
    /// session UUID.
    ///
    /// **Nothing forks here.** This method just registers the record;
    /// materialization happens on the fork's *first dispatch*, which resumes
    /// the parent session with `--fork-session` (see
    /// `claude_code::build_args`). Not a design preference but a CLI
    /// constraint: Claude has no copy-a-session operation, and a fork
    /// invocation with an empty prompt is refused outright ("Provide a prompt
    /// to continue…" — probed 2026-08-10 @ 2.1.226, harness-behavior §3.5).
    /// A branch can only come into existence *as* a turn, which is why the
    /// caller couples fork registration to a send.
    ///
    /// **This is not a complete eligibility check.** It validates two things:
    /// that the source's harness supports the deferred fork lifecycle, and that
    /// the source carries a locator of the right shape. It deliberately does
    /// **not** check that the source's session actually exists on disk — core
    /// has no business reading harness filesystems, exactly as the
    /// `register_attached_*` methods leave file validation to their caller. A
    /// never-dispatched Claude agent has a locator and no session file, and
    /// forking it here succeeds while producing an unmaterializable fork. The
    /// **caller owns the resumability check** (the app layer, via its
    /// per-harness session-file resolution) and owns the friendly "no session
    /// to branch from yet" error the user sees.
    ///
    /// The derived name is `<source>-fork`, disambiguated as `-fork-2`,
    /// `-fork-3`, … against the project's canonicalized uniqueness rule. (Not
    /// "X (forked)": names must match `^[A-Za-z0-9_-]+$`.) Same TOCTOU
    /// caveat as [`Self::register_agent`] — callers serialize.
    pub fn fork_agent(&self, source_agent_id: AgentId) -> Result<AgentRecord> {
        let agents = self.list_agents()?;
        let source = agents
            .iter()
            .find(|a| a.id == source_agent_id)
            .ok_or(CoreError::AgentNotFound(source_agent_id))?;

        if !source.harness.supports_session_fork() {
            return Err(CoreError::SessionForkUnsupported {
                harness: source.harness,
            });
        }
        // `supports_session_fork` is Claude-only, and Claude locators are always
        // the `Uuid` shape — so `as_uuid()` failing here means no locator at all.
        let parent_session = source
            .session_locator
            .as_ref()
            .and_then(SessionLocator::as_uuid)
            .ok_or(CoreError::SessionForkSourceMissing {
                agent_id: source_agent_id,
            })?;

        let name = derive_fork_name(&agents, &source.name);
        let harness = source.harness;
        let model = source.model.clone();
        let effort = source.effort.clone();
        let profiles = source.profiles.clone();
        self.register_agent_inner(NewAgent {
            name: &name,
            harness,
            session_locator: Some(SessionLocator::Uuid(Uuid::now_v7())),
            model,
            effort,
            profiles,
            forked_from_session: Some(parent_session),
            // Where the parent's transcript lives, captured now because it is
            // invariant from here on (see the field's doc) and cannot be safely
            // re-derived at dispatch time once projects can move.
            forked_from_session_home: Some(
                source
                    .effective_session_directory(&self.directory)
                    .to_owned(),
            ),
        })
    }

    /// Register an attached **Claude Code** agent — one that wraps an
    /// already-existing harness session (e.g., a session the user started
    /// outside Switchboard). The provided `session_id` is the existing
    /// `~/.claude/projects/<encoded-cwd>/<uuid>.jsonl` filename. Caller
    /// (commands layer) is responsible for validating the session file
    /// exists; this method only persists the record.
    pub fn register_attached_claude_agent(
        &self,
        name: &str,
        session_id: Uuid,
        model: Option<String>,
        effort: Option<String>,
    ) -> Result<AgentRecord> {
        self.register_agent_inner(NewAgent {
            name,
            harness: HarnessKind::ClaudeCode,
            session_locator: Some(SessionLocator::Uuid(session_id)),
            model,
            effort,
            profiles: AgentProfiles::default(),
            forked_from_session: None,
            forked_from_session_home: None,
        })
    }

    /// Register an attached **Codex** agent — one that wraps an existing Codex
    /// session. The `thread_id` and partition-date (parsed from the existing
    /// rollout file's name and directory) are the agent's session locator and
    /// are written straight onto the record — no sidecar, no pre-generated-id
    /// ordering. The commands layer locates and validates the rollout file
    /// before calling this.
    pub fn register_attached_codex_agent(
        &self,
        name: &str,
        thread_id: String,
        partition_date: chrono::NaiveDate,
        model: Option<String>,
        effort: Option<String>,
    ) -> Result<AgentRecord> {
        self.register_agent_inner(NewAgent {
            name,
            harness: HarnessKind::Codex,
            session_locator: Some(SessionLocator::Codex {
                thread_id,
                partition_date,
            }),
            model,
            effort,
            profiles: AgentProfiles::default(),
            forked_from_session: None,
            forked_from_session_home: None,
        })
    }

    /// Register an attached **Antigravity** agent — one that wraps an existing
    /// server-assigned conversation. Now mirrors the Claude
    /// caller-controlled-UUID pattern: the conversation UUID is the agent's
    /// session locator and is written straight onto the record, so there is no
    /// sidecar and no pre-generated-id ordering dance. The commands layer
    /// validates the conversation directory exists before calling this.
    ///
    /// Takes neither `model` nor `effort`: Antigravity supports neither (its
    /// model is harness-owned global config, and effort is folded into the
    /// model name), so both invariants are encoded in the signature.
    pub fn register_attached_antigravity_agent(
        &self,
        name: &str,
        conversation_id: Uuid,
    ) -> Result<AgentRecord> {
        self.register_agent_inner(NewAgent {
            name,
            harness: HarnessKind::Antigravity,
            session_locator: Some(SessionLocator::Uuid(conversation_id)),
            model: None,
            effort: None,
            profiles: AgentProfiles::default(),
            forked_from_session: None,
            forked_from_session_home: None,
        })
    }

    /// Shared validation + JSONL append. Caller decides the `session_locator`
    /// strategy (create vs. attach, per-harness) and the fork provenance; the
    /// `AgentId` is minted here (UUID v7) because it never varied by caller.
    /// Private to enforce the public
    /// surface invariants: create-path uses `register_agent`, attach-path uses
    /// the harness-specific `register_attached_*` methods, so a Claude attach
    /// without a `session_locator` (or a Codex attach with one) is
    /// unrepresentable at the API boundary.
    ///
    /// This is also the single chokepoint that enforces every **capability**
    /// invariant at the persistence boundary — model, effort, and fork
    /// provenance — mirroring [`Self::set_session_locator`]'s `is_valid_for`
    /// guard. The attach variants already make unsupported combinations
    /// unrepresentable in their signatures; this catches the generic create
    /// path (and any future caller) so an unsupported selection or a fork token
    /// on a harness that can't use it never reaches `registry.jsonl`,
    /// regardless of whether a higher layer remembered to check.
    ///
    /// Validation lives **here**, not on [`NewAgent`]: the value of a single
    /// chokepoint is that there is exactly one place an invariant can be
    /// checked, and a validating constructor plus a validating function would
    /// split that in two.
    fn register_agent_inner(&self, spec: NewAgent<'_>) -> Result<AgentRecord> {
        let NewAgent {
            name,
            harness,
            session_locator,
            model,
            effort,
            profiles,
            forked_from_session,
            forked_from_session_home,
        } = spec;
        validate_name(name)?;
        // Normalize **before** the capability check: a blank selection means
        // "unset," which is allowed on any harness — it must not trip the
        // capability error (e.g. a whitespace effort is "no effort,"
        // not an unsupported effort).
        let model = normalize_selection(model);
        let effort = normalize_selection(effort);
        let mut profiles = profiles;
        if let Some(secondary) = &mut profiles.secondary {
            secondary.model = normalize_selection(secondary.model.take());
            secondary.effort = normalize_selection(secondary.effort.take());
        }
        // These four capability gates are currently unreachable: every supported
        // harness drives both axes, so neither `supports_*_selection` returns
        // false for any variant. They are retained deliberately, not left as dead
        // code — they are the forcing function that makes the next harness's
        // capabilities a decision rather than an accident, and the axes really are
        // independent (a harness with model control but no effort control existed
        // here until Gemini was removed). The tests that exercised the `Err` paths
        // went with that harness; there is no way to fabricate an unsupporting
        // variant, so the paths stay covered by construction rather than by test.
        if model.is_some() && !harness.supports_model_selection() {
            return Err(CoreError::SelectionUnsupported {
                harness,
                axis: SelectionAxis::Model,
            });
        }
        if effort.is_some() && !harness.supports_effort_selection() {
            return Err(CoreError::SelectionUnsupported {
                harness,
                axis: SelectionAxis::Effort,
            });
        }
        // See `set_agent_profiles` for why an effort with no model is refused
        // here and why its mirror (a model that requires an effort) is not.
        if harness.effort_requires_model() && effort.is_some() && model.is_none() {
            return Err(CoreError::EffortWithoutModel { harness });
        }
        if let Some(secondary) = &profiles.secondary {
            if secondary.model.is_some() && !harness.supports_model_selection() {
                return Err(CoreError::SelectionUnsupported {
                    harness,
                    axis: SelectionAxis::Model,
                });
            }
            if secondary.effort.is_some() && !harness.supports_effort_selection() {
                return Err(CoreError::SelectionUnsupported {
                    harness,
                    axis: SelectionAxis::Effort,
                });
            }
            if harness.effort_requires_model()
                && secondary.effort.is_some()
                && secondary.model.is_none()
            {
                return Err(CoreError::EffortWithoutModel { harness });
            }
        }
        if profiles.secondary.is_none() {
            profiles.active = AgentProfileSlot::Primary;
        }
        // Fork provenance is only meaningful to a harness whose fork is the
        // deferred kind [`HarnessKind::supports_session_fork`] describes — the
        // field *is* that harness's materialization token. On any other harness
        // no adapter would ever read it, so the agent would silently start a
        // fresh session while the registry claimed it was a branch: a
        // data-level lie with no failure signal. Reject it here so the field
        // doc's "only fork-capable harnesses carry `Some`" is enforced by the
        // boundary rather than held by convention.
        if forked_from_session.is_some() && !harness.supports_session_fork() {
            return Err(CoreError::SessionForkUnsupported { harness });
        }
        check_name_unique(&self.list_agents()?, name, None)?;

        let record = AgentRecord {
            session_home: None,
            forked_from_session_home,
            id: Uuid::now_v7(),
            project_id: self.id,
            name: name.to_owned(),
            harness,
            session_locator,
            model,
            effort,
            profiles,
            forked_from_session,
            created_at: Utc::now(),
        };

        append_jsonl(&self.registry_path, &record)?;
        Ok(record)
    }

    /// Load every agent record, **validating cross-field invariants on the way
    /// in**. See [`AgentRecord::validate`] for why serde alone can't, and
    /// [`reject_fork_provenance_cycles`] for the one check no single record can
    /// answer. A registry that contradicts itself fails the load rather than
    /// yielding a partially-trustworthy roster — matching how a corrupt JSONL
    /// line is already treated.
    pub fn list_agents(&self) -> Result<Vec<AgentRecord>> {
        read_registry(&self.registry_path, self.id)
    }

    /// Remove an agent from the registry by id, rewriting `registry.jsonl`
    /// without the record. Returns whether a record was actually removed, so a
    /// stale or double remove is detectable rather than a silent no-op. Touches
    /// only the registry — the caller owns sidecar cleanup and any actor
    /// teardown.
    pub fn remove_agent(&self, agent_id: crate::agent::AgentId) -> Result<bool> {
        let mut agents = self.list_agents()?;
        let before = agents.len();
        agents.retain(|a| a.id != agent_id);
        if agents.len() == before {
            return Ok(false);
        }
        write_jsonl(&self.registry_path, &agents)?;
        Ok(true)
    }

    /// Rename an agent in the registry. Validates the new name's format and its
    /// canonicalized uniqueness against the *other* agents (self excluded, so
    /// re-saving the same name — or a case/hyphen variant — is allowed), then
    /// rewrites `registry.jsonl`. Returns the updated record.
    pub fn rename_agent(
        &self,
        agent_id: crate::agent::AgentId,
        new_name: &str,
    ) -> Result<AgentRecord> {
        validate_name(new_name)?;
        let mut agents = self.list_agents()?;
        let idx = agents
            .iter()
            .position(|a| a.id == agent_id)
            .ok_or(CoreError::AgentNotFound(agent_id))?;
        check_name_unique(&agents, new_name, Some(agent_id))?;
        new_name.clone_into(&mut agents[idx].name);
        let updated = agents[idx].clone();
        write_jsonl(&self.registry_path, &agents)?;
        Ok(updated)
    }

    /// Adopt an agent record moved from another project: re-stamp its
    /// `project_id` to this project, optionally record its `session_home`, and
    /// append it to this registry. Returns the adopted record.
    ///
    /// **The sanctioned mutator of `project_id`.** [`AgentRecord::project_id`]
    /// is otherwise immutable — every other write path stamps it once at
    /// registration. A move is the one operation that changes it, and routing
    /// that through here keeps the invariant checkable ("who writes
    /// `project_id`?" has exactly two answers) instead of dissolving into a
    /// generic update API. The source half of a move is the existing
    /// [`Self::remove_agent`]; this is the target half.
    ///
    /// **Idempotent by `agent_id`, because move recovery re-drives every step**
    /// — but only for a row that *matches*. The intended record is built first
    /// and an already-present id is accepted as "this step already ran" only
    /// when the stored row equals it exactly; anything else is
    /// [`CoreError::AgentAdoptionConflict`], leaving the move blocked for
    /// repair. Trusting a same-id row unconditionally would let a stray or
    /// corrupt record be reported as a completed adoption, after which the move
    /// deletes the source copy — the "silently pick a winner" outcome the
    /// recovery design exists to prevent. Equality is over the whole record
    /// because `project_id` and `session_home` are already resolved on the
    /// intended one: a differing `session_home` is precisely the divergence
    /// worth catching, so it must not be excluded from the comparison.
    ///
    /// **Name uniqueness is enforced here, not assumed.** `register_agent` and
    /// `rename_agent` are the only other writers and both check; a move that
    /// appended blind could seat two agents whose names collide under the
    /// canonical rule (case-insensitive, hyphens as underscores), producing a
    /// roster the ordinary APIs would have refused and ambiguous name
    /// resolution in the UI and workflows. The caller is expected to surface
    /// [`CoreError::DuplicateAgentName`] as a "rename one of them first"
    /// refusal *before* it starts surgery — reaching this check mid-move means
    /// the target roster changed underneath, which is why the check lives at
    /// the write and not only in the caller.
    ///
    /// `session_home` is a **proposal, applied only when the record has none**;
    /// see [`AgentRecord::session_home`] for what belongs there and, crucially,
    /// what must not (a re-canonicalized or catalog-resolved path). A value
    /// already on the record always wins, because the transcript never leaves
    /// the directory it was first encoded from — so a second move preserves the
    /// first move's answer whatever the caller computes. A proposal that
    /// *contradicts* the recorded value is
    /// [`CoreError::SessionHomeContradiction`] rather than either overwriting
    /// it (which strands the agent permanently — harness-behavior §3.5b) or
    /// silently discarding it (which would hide a caller that computed the
    /// wrong directory, letting it stay wrong everywhere else it is used).
    /// Enforcing this here rather than trusting the caller is deliberate: the
    /// contract belongs to the field, and the caller's own rule for deriving it
    /// lives several calls away.
    ///
    /// **Safe under move recovery, which is where a spurious refusal would hurt
    /// most.** Recovery re-drives adoption from scratch and recomputes the
    /// proposal each time (the intent record carries only the agent and the two
    /// projects, never intermediate values). That recomputation is
    /// deterministic while it matters: both projects are held under the move's
    /// maintenance gate, so the directory identities and session-file existence
    /// it reads cannot change between attempts. A re-drive therefore proposes
    /// the same value it proposed before and matches — it cannot wedge a
    /// half-finished move by refusing itself.
    pub fn adopt_agent(
        &self,
        record: &AgentRecord,
        session_home: Option<PathBuf>,
    ) -> Result<AgentRecord> {
        let agents = self.list_agents()?;
        let session_home = match (&record.session_home, session_home) {
            (Some(recorded), Some(proposed)) if *recorded != proposed => {
                return Err(CoreError::SessionHomeContradiction {
                    agent_id: record.id,
                    recorded: recorded.clone(),
                    proposed,
                });
            }
            // The recorded value wins whenever there is one: the transcript did
            // not move, so neither may this.
            (Some(recorded), _) => Some(recorded.clone()),
            (None, proposed) => proposed,
        };
        let adopted = AgentRecord {
            project_id: self.id,
            session_home,
            ..record.clone()
        };
        if let Some(existing) = agents.iter().find(|a| a.id == adopted.id) {
            if *existing == adopted {
                return Ok(adopted);
            }
            return Err(CoreError::AgentAdoptionConflict {
                agent_id: adopted.id,
            });
        }
        check_name_unique(&agents, &adopted.name, None)?;
        adopted.validate()?;
        append_jsonl(&self.registry_path, &adopted)?;
        Ok(adopted)
    }

    /// Set one agent's `session_locator` in place, rewriting `registry.jsonl`
    /// with the new value and every other record (and their order) preserved.
    /// Returns the updated record.
    ///
    /// This is the registry's only in-place field mutation beyond `rename_agent`.
    /// It exists for the runtime-capture path: Codex/Antigravity learn their
    /// session locator on first dispatch (and Antigravity can re-learn it on a
    /// fork-and-heal), and the captured locator is identity that belongs on the
    /// record. Same atomic full-rewrite + concurrency contract as
    /// `remove_agent`/`rename_agent` — callers serialize via the app's
    /// `registry_write` mutex. Deliberately *not* a generic update API; this is
    /// the one mutation the capture path needs.
    pub fn set_session_locator(
        &self,
        agent_id: crate::agent::AgentId,
        locator: SessionLocator,
    ) -> Result<AgentRecord> {
        let mut agents = self.list_agents()?;
        let idx = agents
            .iter()
            .position(|a| a.id == agent_id)
            .ok_or(CoreError::AgentNotFound(agent_id))?;
        // Reject a locator whose shape doesn't match the agent's harness (e.g.
        // a Codex locator on a Claude agent). This is the persistence-boundary
        // guard: an adapter capture bug would otherwise durably store a record
        // that silently fails to resume. The enum makes intra-variant invalid
        // states unrepresentable; this closes the harness↔variant gap.
        let harness = agents[idx].harness;
        if !locator.is_valid_for(harness) {
            return Err(CoreError::SessionLocatorHarnessMismatch { agent_id, harness });
        }
        agents[idx].session_locator = Some(locator);
        let updated = agents[idx].clone();
        write_jsonl(&self.registry_path, &agents)?;
        Ok(updated)
    }

    /// Atomically replace an agent's primary and optional secondary execution
    /// profiles. One registry rewrite prevents a model/effort pair from being
    /// left half-updated if persistence fails. Disabling the secondary profile
    /// also returns the agent to Primary.
    pub fn set_agent_profiles(
        &self,
        agent_id: crate::agent::AgentId,
        primary: AgentProfile,
        secondary: Option<AgentProfile>,
    ) -> Result<AgentRecord> {
        let mut agents = self.list_agents()?;
        let idx = agents
            .iter()
            .position(|a| a.id == agent_id)
            .ok_or(CoreError::AgentNotFound(agent_id))?;
        let harness = agents[idx].harness;
        let normalize_profile = |mut profile: AgentProfile| -> AgentProfile {
            profile.model = normalize_selection(profile.model);
            profile.effort = normalize_selection(profile.effort);
            profile
        };
        let primary = normalize_profile(primary);
        let secondary = secondary.map(normalize_profile);
        for profile in std::iter::once(&primary).chain(secondary.iter()) {
            if profile.model.is_some() && !harness.supports_model_selection() {
                return Err(CoreError::SelectionUnsupported {
                    harness,
                    axis: SelectionAxis::Model,
                });
            }
            if profile.effort.is_some() && !harness.supports_effort_selection() {
                return Err(CoreError::SelectionUnsupported {
                    harness,
                    axis: SelectionAxis::Effort,
                });
            }
            // An effort with no model is incoherent **only where the harness
            // derives its levels from the model** — see
            // `HarnessKind::effort_requires_model`. Claude and Codex emit their
            // effort flag independently, so "default model at high effort" is
            // valid for them and must keep persisting; gating on the capability
            // rather than applying this unconditionally is what preserves that.
            //
            // Where it does apply (Antigravity), refuse to store rather than
            // silently drop the flag at dispatch, which would leave the record
            // asserting a selection the turn never applied. The mirror case — a
            // model that *requires* an effort and has none — is deliberately NOT
            // checked here: that would mean duplicating the model catalog into
            // core, and `agy` already rejects it pre-dispatch, quota-free, with
            // a message naming the valid levels, surfaced verbatim.
            if harness.effort_requires_model()
                && profile.effort.is_some()
                && profile.model.is_none()
            {
                return Err(CoreError::EffortWithoutModel { harness });
            }
        }
        agents[idx].model = primary.model;
        agents[idx].effort = primary.effort;
        agents[idx].profiles.secondary = secondary;
        if agents[idx].profiles.secondary.is_none() {
            agents[idx].profiles.active = AgentProfileSlot::Primary;
        }
        let updated = agents[idx].clone();
        write_jsonl(&self.registry_path, &agents)?;
        Ok(updated)
    }

    /// Select which configured profile future sends capture. Existing in-flight
    /// and queued work is unaffected because each accepted send owns a snapshot.
    pub fn set_active_agent_profile(
        &self,
        agent_id: crate::agent::AgentId,
        active: AgentProfileSlot,
    ) -> Result<AgentRecord> {
        let mut agents = self.list_agents()?;
        let idx = agents
            .iter()
            .position(|a| a.id == agent_id)
            .ok_or(CoreError::AgentNotFound(agent_id))?;
        if active == AgentProfileSlot::Secondary && agents[idx].profiles.secondary.is_none() {
            return Err(CoreError::SecondaryProfileMissing(agent_id));
        }
        agents[idx].profiles.active = active;
        let updated = agents[idx].clone();
        write_jsonl(&self.registry_path, &agents)?;
        Ok(updated)
    }

    /// Rewrite `registry.jsonl` so its records appear in `ordered_ids` order.
    /// Physical record order is the roster's canonical, user-visible order
    /// (sidebar cards, compose chips, ⌘1..9), so reordering is a full rewrite
    /// like the other mutations — same atomic + `registry_write`-serialized
    /// contract. `ordered_ids` must be an exact permutation of the current
    /// roster; a stale list (an agent added or removed since the caller read
    /// the roster) is rejected rather than silently dropping or duplicating
    /// records. Returns the records in their new order.
    pub fn reorder_agents(
        &self,
        ordered_ids: &[crate::agent::AgentId],
    ) -> Result<Vec<AgentRecord>> {
        let agents = self.list_agents()?;
        let mismatch = CoreError::ReorderRosterMismatch {
            expected: agents.len(),
            provided: ordered_ids.len(),
        };
        if ordered_ids.len() != agents.len() {
            return Err(mismatch);
        }
        let mut by_id: std::collections::HashMap<crate::agent::AgentId, AgentRecord> =
            agents.into_iter().map(|a| (a.id, a)).collect();
        let mut reordered = Vec::with_capacity(ordered_ids.len());
        for id in ordered_ids {
            // A duplicate id hits this on its second occurrence (already
            // removed), so the permutation check needs no separate pass.
            let Some(record) = by_id.remove(id) else {
                return Err(mismatch);
            };
            reordered.push(record);
        }
        write_jsonl(&self.registry_path, &reordered)?;
        Ok(reordered)
    }
}

/// Canonicalized-uniqueness check shared by register (`exclude` = `None`) and
/// rename (`exclude` = the renamed agent's id, so it doesn't collide with
/// itself). Per system-design §4, names collide case-insensitively and with
/// hyphens treated as underscores.
fn check_name_unique(
    agents: &[AgentRecord],
    name: &str,
    exclude: Option<crate::agent::AgentId>,
) -> Result<()> {
    let canonical = canonicalize_for_uniqueness(name);
    for existing in agents {
        if Some(existing.id) == exclude {
            continue;
        }
        if canonicalize_for_uniqueness(&existing.name) == canonical {
            return Err(CoreError::DuplicateAgentName {
                name: name.to_owned(),
                existing: existing.name.clone(),
            });
        }
    }
    Ok(())
}

/// Derive an unused name for a fork of `source_name`: `<source>-fork`, then
/// `-fork-2`, `-fork-3`, … until one is free under the project's canonicalized
/// uniqueness rule.
///
/// Suffixing (rather than a parenthesized "(forked)") is forced by
/// [`validate_name`]'s `^[A-Za-z0-9_-]+$`. Forking a fork therefore yields
/// `x-fork-fork` — accepted as honest lineage rather than special-cased; the
/// user can rename.
///
/// The search is bounded by the roster size: each candidate is distinct, so at
/// most `agents.len()` of them can collide and one past that is always free
/// (pigeonhole). The fallback is therefore unreachable — and note it returns
/// `base`, a name already known to collide, so it is a *fail-loud* fallback,
/// not a usable answer: `register_agent_inner` rejects it as
/// `DuplicateAgentName`. Preferred over panicking, and asserted in debug builds
/// so a broken bound surfaces in tests rather than as a puzzling duplicate.
fn derive_fork_name(agents: &[AgentRecord], source_name: &str) -> String {
    let base = format!("{source_name}-fork");
    if check_name_unique(agents, &base, None).is_ok() {
        return base;
    }
    let candidate = (2..=agents.len() + 2)
        .map(|n| format!("{base}-{n}"))
        .find(|candidate| check_name_unique(agents, candidate, None).is_ok());
    debug_assert!(
        candidate.is_some(),
        "pigeonhole violated: {} distinct candidates against {} agents",
        agents.len() + 1,
        agents.len()
    );
    candidate.unwrap_or(base)
}

/// Load a `Project` from disk. Reads the per-project config.yaml; the caller has
/// already located the project root (e.g., via `Directory::open_project`).
pub(crate) fn load(directory: &Path, id: ProjectId, root: PathBuf) -> Result<Project> {
    let config_path = root.join(CONFIG_FILE);
    let config = read_yaml::<ProjectConfig>(&config_path)?;
    if config.version != PROJECT_CONFIG_VERSION {
        return Err(CoreError::UnsupportedConfigVersion {
            path: config_path,
            found: config.version,
            expected: PROJECT_CONFIG_VERSION,
        });
    }
    let registry_path = root.join(REGISTRY_FILE);
    Ok(Project {
        id,
        directory: directory.to_owned(),
        config,
        root,
        registry_path,
    })
}

/// Read and validate a project's agent registry from its path alone.
///
/// **Deliberately independent of the working directory.** `registry.jsonl`
/// lives under the store root, keyed by project id, so a project whose catalog
/// entry is missing or ambiguous still has a readable roster. That is what lets
/// the session-uniqueness scans stay whole when one catalog row is damaged:
/// they need the registry and a display name, never a cwd. Routing them through
/// a `Project` would manufacture a dependency on resolution that the read does
/// not have.
///
/// **A missing `registry.jsonl` is corruption, not an empty roster.**
/// `create_on_disk` creates it with `create_new`, so every project that exists
/// has one; `read_jsonl` would otherwise map its absence to `Ok(vec![])` and the
/// session-id uniqueness scans — the whole reason this read is catalog-free —
/// would silently pass over a project whose agents they could not see. Same
/// posture the store already takes on `projects.jsonl` and `directories.jsonl`.
///
/// Shared with [`Project::list_agents`] so both paths apply the same
/// cross-field validation rather than one of them growing a laxer copy.
pub(crate) fn read_registry(
    registry_path: &Path,
    project_id: ProjectId,
) -> Result<Vec<AgentRecord>> {
    if !registry_path.exists() {
        return Err(CoreError::MissingAppendOnlyFile {
            path: registry_path.to_path_buf(),
        });
    }
    let agents: Vec<AgentRecord> = read_jsonl(registry_path)?;
    for agent in &agents {
        agent.validate()?;
        if agent.project_id != project_id {
            return Err(CoreError::AgentProjectMismatch {
                registry: registry_path.to_path_buf(),
                agent_id: agent.id,
                claimed: agent.project_id,
                actual: project_id,
            });
        }
    }
    reject_duplicate_identities(&agents, registry_path)?;
    reject_fork_provenance_cycles(&agents)?;
    Ok(agents)
}

/// Reject a registry containing two records that share an identity.
///
/// Runs **before** the provenance walk, which builds a session-keyed map: with
/// duplicate locators that map silently keeps one of the pair and the cycle
/// result becomes insertion-order dependent, so uniqueness has to hold first.
///
/// Scope is this project's registry. The write-side check
/// (`check_claude_session_id_unique`) is directory-wide, which is broader — but
/// core cannot see sibling projects, and re-scanning every project in the
/// directory on every open would cost O(projects × agents) for a
/// corruption-only case. Within-registry is where the concrete harm lives.
fn reject_duplicate_identities(agents: &[AgentRecord], registry: &Path) -> Result<()> {
    let mut ids: std::collections::HashMap<uuid::Uuid, uuid::Uuid> =
        std::collections::HashMap::new();
    let mut sessions: std::collections::HashMap<uuid::Uuid, uuid::Uuid> =
        std::collections::HashMap::new();
    for agent in agents {
        if let Some(first) = ids.insert(agent.id, agent.id) {
            return Err(CoreError::DuplicateAgentIdentity {
                registry: registry.to_owned(),
                field: "agent id",
                first,
                second: agent.id,
            });
        }
        if let Some(session) = agent
            .session_locator
            .as_ref()
            .and_then(SessionLocator::as_uuid)
            && let Some(first) = sessions.insert(session, agent.id)
        {
            return Err(CoreError::DuplicateAgentIdentity {
                registry: registry.to_owned(),
                field: "harness session",
                first,
                second: agent.id,
            });
        }
    }
    Ok(())
}

/// Reject a registry whose fork provenance loops back on itself.
///
/// Each agent has at most one outgoing edge (`forked_from_session` names the
/// parent's session), so the graph is a forest unless it is corrupt — walking
/// from any node either terminates or revisits, and revisiting is the only
/// failure. Per-record validation cannot see this: every individual record in a
/// two-agent loop is internally consistent.
fn reject_fork_provenance_cycles(agents: &[AgentRecord]) -> Result<()> {
    let by_session: std::collections::HashMap<uuid::Uuid, &AgentRecord> = agents
        .iter()
        .filter_map(|a| {
            a.session_locator
                .as_ref()
                .and_then(SessionLocator::as_uuid)
                .map(|uuid| (uuid, a))
        })
        .collect();
    for start in agents {
        let mut seen = std::collections::HashSet::new();
        let mut current = start;
        while let Some(parent_session) = current.forked_from_session {
            if !seen.insert(current.id) {
                return Err(CoreError::ForkProvenanceCycle { agent_id: start.id });
            }
            match by_session.get(&parent_session) {
                Some(parent) => current = parent,
                // Parent not in this registry (deleted, or another directory):
                // the chain ends, which is the normal case for an ordinary fork
                // whose source was removed.
                None => break,
            }
        }
    }
    Ok(())
}

/// Create a new project's on-disk artifacts (config.yaml + empty registry.jsonl).
/// The caller is responsible for appending the index entry — and, under the
/// legacy `Directory` layout, for rolling back the directory if that append
/// fails.
///
/// `directory_id` is `None` only for the legacy layout, where the owning
/// directory was implied by the path; see [`ProjectConfig::directory_id`].
pub(crate) fn create_on_disk(
    directory: &Path,
    directory_id: Option<DirectoryId>,
    projects_dir: &Path,
    name: &str,
) -> Result<(ProjectSummary, Project)> {
    let id = Uuid::now_v7();
    let root = projects_dir.join(id.to_string());
    create_dir_all(&root).map_err(|e| CoreError::io(&root, e))?;

    let created_at = Utc::now();
    let config = ProjectConfig {
        version: PROJECT_CONFIG_VERSION,
        name: name.to_owned(),
        created_at,
        directory_id,
    };
    write_yaml(&root.join(CONFIG_FILE), &config)?;

    // Touch registry.jsonl so the file exists even before any agents are
    // registered. `create_new` (not `create`) so we fail fast if a stale
    // registry already sits at this path — that would only happen if a
    // prior `create_project` partially succeeded and rollback failed to
    // remove the project dir; under that condition we want a hard error,
    // not silent truncation of a registry that might still have data.
    let registry_path = root.join(REGISTRY_FILE);
    std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&registry_path)
        .map_err(|e| CoreError::io(&registry_path, e))?;

    let summary = ProjectSummary {
        id,
        name: name.to_owned(),
        created_at,
    };
    let project = Project {
        id,
        directory: directory.to_owned(),
        config,
        root,
        registry_path,
    };
    Ok((summary, project))
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;
    use tempfile::TempDir;

    fn fresh_project() -> (TempDir, Project) {
        let tmp = TempDir::new().unwrap();
        let projects_dir = tmp.path().join("projects");
        create_dir_all(&projects_dir).unwrap();
        let (_summary, project) =
            create_on_disk(tmp.path(), None, &projects_dir, "test-project").unwrap();
        (tmp, project)
    }

    #[test]
    fn register_then_list_agent_roundtrips() {
        let (_tmp, project) = fresh_project();
        let record = project
            .register_agent("assistant", HarnessKind::ClaudeCode, None, None)
            .unwrap();
        assert_eq!(record.name, "assistant");
        assert_eq!(record.project_id, project.id);
        assert!(record.session_locator.is_some()); // ClaudeCode pre-generates a UUID locator.

        let listed = project.list_agents().unwrap();
        assert_eq!(listed, vec![record]);
    }

    #[test]
    fn a_registry_record_contradicting_its_harness_fails_the_load() {
        // Serde checks fields in isolation, so a hand-edited or corrupted line
        // can pair fork provenance with a harness that cannot fork. That is not
        // inert: the harness-agnostic dispatch gates read the field for every
        // agent and would treat this record as a branch waiting to materialize.
        let (_tmp, project) = fresh_project();
        let claude = project
            .register_agent("alice", HarnessKind::ClaudeCode, None, None)
            .unwrap();
        let mut codex = project
            .register_agent("bob", HarnessKind::Codex, None, None)
            .unwrap();
        codex.forked_from_session = Some(uuid::Uuid::now_v7());
        crate::io::write_jsonl(&project.registry_path, &[claude, codex]).unwrap();

        let err = project
            .list_agents()
            .expect_err("a record contradicting its own harness must not load");
        assert!(
            matches!(err, CoreError::SessionForkUnsupported { .. }),
            "got: {err:?}"
        );
    }

    #[test]
    fn two_agents_sharing_an_id_or_a_session_fail_the_load() {
        // Duplicate ids collapse in the app cache — two roster rows sharing one
        // runtime and one actor. Duplicate session locators mean two agents
        // driving one harness conversation, and they also silently corrupt the
        // provenance walk, whose session-keyed map keeps only one of the pair.
        let (_tmp, project) = fresh_project();
        let a = project
            .register_agent("alice", HarnessKind::ClaudeCode, None, None)
            .unwrap();
        let b = project
            .register_agent("bob", HarnessKind::ClaudeCode, None, None)
            .unwrap();

        let mut dup_id = b.clone();
        dup_id.id = a.id;
        crate::io::write_jsonl(&project.registry_path, &[a.clone(), dup_id]).unwrap();
        assert!(matches!(
            project
                .list_agents()
                .expect_err("duplicate id must not load"),
            CoreError::DuplicateAgentIdentity {
                field: "agent id",
                ..
            }
        ),);

        let mut dup_session = b.clone();
        dup_session.session_locator = a.session_locator.clone();
        crate::io::write_jsonl(&project.registry_path, &[a, dup_session]).unwrap();
        assert!(matches!(
            project
                .list_agents()
                .expect_err("duplicate session must not load"),
            CoreError::DuplicateAgentIdentity {
                field: "harness session",
                ..
            }
        ),);
    }

    #[test]
    fn a_record_claiming_another_project_fails_the_load() {
        // Not a label: dispatch resolves an agent's project — and therefore its
        // working directory and journal — from this field, so a mismatched record
        // silently runs the agent's work against a different project's directory
        // whenever that project is also loaded.
        let (_tmp, project) = fresh_project();
        let mut agent = project
            .register_agent("alice", HarnessKind::ClaudeCode, None, None)
            .unwrap();
        agent.project_id = uuid::Uuid::now_v7();
        crate::io::write_jsonl(&project.registry_path, &[agent]).unwrap();

        assert!(matches!(
            project
                .list_agents()
                .expect_err("a misfiled record must not load"),
            CoreError::AgentProjectMismatch { .. }
        ));
    }

    #[test]
    fn fork_provenance_that_loops_fails_the_load() {
        // Two agents naming each other's sessions. Every record is individually
        // consistent, so only a whole-set pass can see it — and it matters
        // because the materializing-fork gate asks each agent's actor whether
        // its parent is mid-turn, so a loop deadlocks both.
        let (_tmp, project) = fresh_project();
        let mut a = project
            .register_agent("alice", HarnessKind::ClaudeCode, None, None)
            .unwrap();
        let mut b = project
            .register_agent("bob", HarnessKind::ClaudeCode, None, None)
            .unwrap();
        let a_session = a
            .session_locator
            .as_ref()
            .and_then(SessionLocator::as_uuid)
            .unwrap();
        let b_session = b
            .session_locator
            .as_ref()
            .and_then(SessionLocator::as_uuid)
            .unwrap();
        a.forked_from_session = Some(b_session);
        b.forked_from_session = Some(a_session);
        crate::io::write_jsonl(&project.registry_path, &[a, b]).unwrap();

        let err = project
            .list_agents()
            .expect_err("a provenance cycle must not load");
        assert!(
            matches!(err, CoreError::ForkProvenanceCycle { .. }),
            "got: {err:?}"
        );
    }

    #[test]
    fn an_ordinary_fork_chain_still_loads() {
        // The check must not mistake a legitimate chain for a cycle: a fork of a
        // fork walks two edges and terminates.
        let (_tmp, project) = fresh_project();
        let parent = project
            .register_agent("alice", HarnessKind::ClaudeCode, None, None)
            .unwrap();
        let child = project.fork_agent(parent.id).unwrap();
        let grandchild = project.fork_agent(child.id).unwrap();

        let loaded = project.list_agents().expect("a fork chain is not a cycle");
        assert_eq!(loaded.len(), 3);
        assert!(loaded.iter().any(|a| a.id == grandchild.id));
    }

    #[test]
    fn fork_agent_branches_from_the_source_session() {
        let (_tmp, project) = fresh_project();
        let source = project
            .register_agent("alice", HarnessKind::ClaudeCode, None, None)
            .unwrap();
        let parent_session = source
            .session_locator
            .as_ref()
            .and_then(SessionLocator::as_uuid)
            .unwrap();

        let fork = project.fork_agent(source.id).unwrap();

        assert_eq!(fork.name, "alice-fork");
        assert_ne!(fork.id, source.id);
        assert_eq!(fork.harness, HarnessKind::ClaudeCode);
        assert_eq!(fork.forked_from_session, Some(parent_session));
        // The fork gets its OWN session to write into — sharing the parent's
        // locator would make both agents drive one session.
        let fork_session = fork
            .session_locator
            .as_ref()
            .and_then(SessionLocator::as_uuid)
            .expect("a fork pre-generates its own Claude locator");
        assert_ne!(fork_session, parent_session);

        // Appended after the source, and the source is untouched.
        let listed = project.list_agents().unwrap();
        assert_eq!(listed, vec![source, fork]);
    }

    #[test]
    fn fork_agent_inherits_model_and_effort() {
        let (_tmp, project) = fresh_project();
        let source = project
            .register_agent(
                "alice",
                HarnessKind::ClaudeCode,
                Some("opus".to_owned()),
                Some("high".to_owned()),
            )
            .unwrap();

        let fork = project.fork_agent(source.id).unwrap();

        assert_eq!(fork.model.as_deref(), Some("opus"));
        assert_eq!(fork.effort.as_deref(), Some("high"));
    }

    #[test]
    fn fork_agent_disambiguates_repeated_forks() {
        let (_tmp, project) = fresh_project();
        let source = project
            .register_agent("alice", HarnessKind::ClaudeCode, None, None)
            .unwrap();

        assert_eq!(project.fork_agent(source.id).unwrap().name, "alice-fork");
        assert_eq!(project.fork_agent(source.id).unwrap().name, "alice-fork-2");
        assert_eq!(project.fork_agent(source.id).unwrap().name, "alice-fork-3");
    }

    #[test]
    fn fork_agent_disambiguates_against_canonicalized_names() {
        // Uniqueness is hyphen↔underscore + case insensitive, so a manually
        // named `Alice_Fork` must push the derived name to `-fork-2` rather
        // than producing a registry the roster considers duplicated.
        let (_tmp, project) = fresh_project();
        let source = project
            .register_agent("alice", HarnessKind::ClaudeCode, None, None)
            .unwrap();
        project
            .register_agent("Alice_Fork", HarnessKind::ClaudeCode, None, None)
            .unwrap();

        assert_eq!(project.fork_agent(source.id).unwrap().name, "alice-fork-2");
    }

    #[test]
    fn fork_agent_rejects_a_harness_that_cannot_branch() {
        let (_tmp, project) = fresh_project();
        // Antigravity pre-generates a locator, so this fails on the capability
        // gate rather than on a missing session — the distinction the two error
        // variants exist to preserve.
        let source = project
            .register_agent("g", HarnessKind::Antigravity, None, None)
            .unwrap();

        let err = project.fork_agent(source.id).unwrap_err();

        assert!(
            matches!(err, CoreError::SessionForkUnsupported { harness } if harness == HarnessKind::Antigravity),
            "got: {err:?}"
        );
        assert_eq!(project.list_agents().unwrap().len(), 1, "no record written");
    }

    #[test]
    fn fork_agent_rejects_a_source_carrying_no_locator() {
        // Unreachable through any supported path *today* — every fork-capable
        // harness pre-generates its locator, so this record shape means the
        // registry is inconsistent. The guard still earns its keep two ways: it
        // keeps a fork with no `--resume` target out of the registry, and if a
        // fork-capable harness ever captures its locator at runtime, this
        // becomes a genuine "not yet — dispatch it once first."
        let (_tmp, project) = fresh_project();
        let source = project
            .register_agent("alice", HarnessKind::ClaudeCode, None, None)
            .unwrap();
        let stranded = AgentRecord {
            session_locator: None,
            ..source
        };
        write_jsonl(&project.registry_path, std::slice::from_ref(&stranded)).unwrap();

        let err = project.fork_agent(stranded.id).unwrap_err();

        assert!(
            matches!(err, CoreError::SessionForkSourceMissing { agent_id } if agent_id == stranded.id),
            "got: {err:?}"
        );
        assert_eq!(project.list_agents().unwrap().len(), 1, "no record written");
    }

    #[test]
    fn registration_rejects_fork_provenance_on_a_harness_that_cannot_fork() {
        // Guards the field's invariant at the persistence boundary rather than
        // trusting `fork_agent` to be the only writer. Reaches the private
        // chokepoint directly because every public path hard-codes `None` —
        // the point is precisely that a *future* public path can't get this
        // wrong silently. Without the guard the record would persist and the
        // Codex adapter would ignore it, starting a fresh session while the
        // registry claimed a branch.
        let (_tmp, project) = fresh_project();

        let err = project
            .register_agent_inner(NewAgent {
                name: "c",
                harness: HarnessKind::Codex,
                session_locator: None,
                model: None,
                effort: None,
                profiles: AgentProfiles::default(),
                forked_from_session: Some(Uuid::now_v7()),
                forked_from_session_home: None,
            })
            .unwrap_err();

        assert!(
            matches!(err, CoreError::SessionForkUnsupported { harness } if harness == HarnessKind::Codex),
            "got: {err:?}"
        );
        assert!(
            project.list_agents().unwrap().is_empty(),
            "a rejected registration must write no record"
        );
    }

    #[test]
    fn registration_allows_fork_provenance_on_a_fork_capable_harness() {
        // The guard's other half: it must not reject the legitimate case.
        let (_tmp, project) = fresh_project();
        let parent = Uuid::now_v7();

        let record = project
            .register_agent_inner(NewAgent {
                name: "a-fork",
                harness: HarnessKind::ClaudeCode,
                session_locator: Some(SessionLocator::Uuid(Uuid::now_v7())),
                model: None,
                effort: None,
                profiles: AgentProfiles::default(),
                forked_from_session: Some(parent),
                forked_from_session_home: None,
            })
            .unwrap();

        assert_eq!(record.forked_from_session, Some(parent));
    }

    #[test]
    fn fork_agent_rejects_an_unknown_source() {
        let (_tmp, project) = fresh_project();
        let missing = Uuid::now_v7();

        let err = project.fork_agent(missing).unwrap_err();

        assert!(
            matches!(err, CoreError::AgentNotFound(id) if id == missing),
            "got: {err:?}"
        );
    }

    #[test]
    fn fork_of_a_fork_branches_from_the_forks_own_session() {
        // Lineage is one hop: the grandchild resumes its *parent's* session, not
        // the original's — otherwise it would lose the middle fork's turns.
        let (_tmp, project) = fresh_project();
        let source = project
            .register_agent("alice", HarnessKind::ClaudeCode, None, None)
            .unwrap();
        let fork = project.fork_agent(source.id).unwrap();

        let grandchild = project.fork_agent(fork.id).unwrap();

        assert_eq!(
            grandchild.forked_from_session,
            fork.session_locator
                .as_ref()
                .and_then(SessionLocator::as_uuid)
        );
        assert_eq!(grandchild.name, "alice-fork-fork");
    }

    #[test]
    fn register_codex_agent_leaves_session_id_none() {
        let (_tmp, project) = fresh_project();
        let record = project
            .register_agent("c", HarnessKind::Codex, None, None)
            .unwrap();
        assert!(record.session_locator.is_none());
    }

    #[test]
    fn register_antigravity_agent_leaves_session_locator_none() {
        // Antigravity assigns the conversation UUID server-side; the adapter
        // captures it post-spawn and the dispatcher persists it onto the
        // registry record. Mirrors Codex's pattern.
        let (_tmp, project) = fresh_project();
        let record = project
            .register_agent("a", HarnessKind::Antigravity, None, None)
            .unwrap();
        assert!(record.session_locator.is_none());
    }

    #[test]
    fn project_name_delegates_to_config() {
        let (_tmp, project) = fresh_project();
        assert_eq!(project.name(), "test-project");
        assert_eq!(project.name(), project.config.name);
    }

    #[test]
    fn register_rejects_duplicate_verbatim() {
        let (_tmp, project) = fresh_project();
        project
            .register_agent("assistant", HarnessKind::ClaudeCode, None, None)
            .unwrap();
        let err = project
            .register_agent("assistant", HarnessKind::ClaudeCode, None, None)
            .unwrap_err();
        assert!(matches!(err, CoreError::DuplicateAgentName { .. }));
    }

    #[test]
    fn register_rejects_duplicate_under_hyphen_underscore_and_case() {
        let (_tmp, project) = fresh_project();
        project
            .register_agent("agent-a", HarnessKind::ClaudeCode, None, None)
            .unwrap();
        for collision in ["agent_a", "Agent-A", "AGENT_A"] {
            let err = project
                .register_agent(collision, HarnessKind::ClaudeCode, None, None)
                .unwrap_err();
            assert!(
                matches!(err, CoreError::DuplicateAgentName { .. }),
                "{collision:?} should collide with 'agent-a'"
            );
        }
    }

    #[test]
    fn remove_agent_drops_target_and_keeps_others() {
        let (_tmp, project) = fresh_project();
        let a = project
            .register_agent("alpha", HarnessKind::ClaudeCode, None, None)
            .unwrap();
        let b = project
            .register_agent("beta", HarnessKind::Codex, None, None)
            .unwrap();
        assert!(project.remove_agent(a.id).unwrap());
        assert_eq!(project.list_agents().unwrap(), vec![b]);
    }

    #[test]
    fn remove_agent_nonexistent_reports_not_removed() {
        let (_tmp, project) = fresh_project();
        project
            .register_agent("alpha", HarnessKind::ClaudeCode, None, None)
            .unwrap();
        assert!(!project.remove_agent(Uuid::now_v7()).unwrap());
        assert_eq!(project.list_agents().unwrap().len(), 1);
    }

    #[test]
    fn removed_name_is_reusable() {
        // Uniqueness is checked against the live registry, so freeing a name by
        // removal lets it be registered again.
        let (_tmp, project) = fresh_project();
        let a = project
            .register_agent("alpha", HarnessKind::ClaudeCode, None, None)
            .unwrap();
        project.remove_agent(a.id).unwrap();
        project
            .register_agent("alpha", HarnessKind::Codex, None, None)
            .expect("name freed by removal");
    }

    #[test]
    fn rename_agent_changes_name_and_persists() {
        let (_tmp, project) = fresh_project();
        let a = project
            .register_agent("alpha", HarnessKind::ClaudeCode, None, None)
            .unwrap();
        let updated = project.rename_agent(a.id, "renamed").unwrap();
        assert_eq!(updated.name, "renamed");
        assert_eq!(updated.id, a.id);
        let listed = project.list_agents().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "renamed");
    }

    #[test]
    fn rename_agent_to_own_name_variant_succeeds() {
        // Self is excluded from the uniqueness check, so a case/hyphen variant
        // of the agent's own name is allowed.
        let (_tmp, project) = fresh_project();
        let a = project
            .register_agent("agent-a", HarnessKind::ClaudeCode, None, None)
            .unwrap();
        let updated = project.rename_agent(a.id, "Agent_A").unwrap();
        assert_eq!(updated.name, "Agent_A");
    }

    #[test]
    fn rename_agent_rejects_canonical_collision_with_another() {
        let (_tmp, project) = fresh_project();
        let a = project
            .register_agent("alpha", HarnessKind::ClaudeCode, None, None)
            .unwrap();
        project
            .register_agent("beta", HarnessKind::Codex, None, None)
            .unwrap();
        let err = project.rename_agent(a.id, "BETA").unwrap_err();
        assert!(matches!(err, CoreError::DuplicateAgentName { .. }));
        // The reject path leaves the registry untouched.
        assert_eq!(project.list_agents().unwrap()[0].name, "alpha");
    }

    #[test]
    fn rename_agent_rejects_invalid_name() {
        let (_tmp, project) = fresh_project();
        let a = project
            .register_agent("alpha", HarnessKind::ClaudeCode, None, None)
            .unwrap();
        let err = project.rename_agent(a.id, "bad name").unwrap_err();
        assert!(matches!(err, CoreError::InvalidName { .. }));
    }

    #[test]
    fn rename_agent_nonexistent_returns_not_found() {
        let (_tmp, project) = fresh_project();
        let err = project.rename_agent(Uuid::now_v7(), "x").unwrap_err();
        assert!(matches!(err, CoreError::AgentNotFound(_)));
    }

    #[test]
    fn set_session_locator_updates_only_target_and_preserves_order() {
        let (_tmp, project) = fresh_project();
        // Three agents in a known order; Codex starts with no locator.
        let a = project
            .register_agent("alpha", HarnessKind::ClaudeCode, None, None)
            .unwrap();
        let b = project
            .register_agent("beta", HarnessKind::Codex, None, None)
            .unwrap();
        let c = project
            .register_agent("gamma", HarnessKind::Antigravity, None, None)
            .unwrap();
        assert!(b.session_locator.is_none());

        let locator = SessionLocator::Codex {
            thread_id: "thread-xyz".to_owned(),
            partition_date: chrono::NaiveDate::from_ymd_opt(2026, 5, 16).unwrap(),
        };
        let updated = project.set_session_locator(b.id, locator.clone()).unwrap();
        assert_eq!(updated.id, b.id);
        assert_eq!(updated.session_locator, Some(locator.clone()));

        let listed = project.list_agents().unwrap();
        // Order preserved: alpha, beta, gamma.
        assert_eq!(
            listed.iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![a.id, b.id, c.id]
        );
        // Only beta changed.
        assert_eq!(listed[0].session_locator, a.session_locator);
        assert_eq!(listed[1].session_locator, Some(locator));
        assert_eq!(listed[2].session_locator, c.session_locator);
    }

    #[test]
    fn reorder_agents_persists_the_new_order() {
        let (_tmp, project) = fresh_project();
        let a = project
            .register_agent("alpha", HarnessKind::ClaudeCode, None, None)
            .unwrap();
        let b = project
            .register_agent("beta", HarnessKind::Codex, None, None)
            .unwrap();
        let c = project
            .register_agent("gamma", HarnessKind::Antigravity, None, None)
            .unwrap();

        let reordered = project.reorder_agents(&[c.id, a.id, b.id]).unwrap();
        assert_eq!(reordered, vec![c.clone(), a.clone(), b.clone()]);

        // The rewrite is durable and field-preserving, not just reflected in
        // the return value.
        let listed = project.list_agents().unwrap();
        assert_eq!(listed, vec![c, a, b]);
    }

    #[test]
    fn reorder_agents_identity_permutation_is_a_valid_noop() {
        let (_tmp, project) = fresh_project();
        let a = project
            .register_agent("alpha", HarnessKind::ClaudeCode, None, None)
            .unwrap();
        let b = project
            .register_agent("beta", HarnessKind::ClaudeCode, None, None)
            .unwrap();
        let reordered = project.reorder_agents(&[a.id, b.id]).unwrap();
        assert_eq!(reordered, vec![a, b]);
    }

    #[test]
    fn reorder_agents_rejects_wrong_length() {
        let (_tmp, project) = fresh_project();
        let a = project
            .register_agent("alpha", HarnessKind::ClaudeCode, None, None)
            .unwrap();
        let b = project
            .register_agent("beta", HarnessKind::ClaudeCode, None, None)
            .unwrap();

        let err = project.reorder_agents(&[a.id]).unwrap_err();
        assert!(matches!(
            err,
            CoreError::ReorderRosterMismatch {
                expected: 2,
                provided: 1
            }
        ));
        // Registry untouched on rejection.
        assert_eq!(
            project
                .list_agents()
                .unwrap()
                .iter()
                .map(|r| r.id)
                .collect::<Vec<_>>(),
            vec![a.id, b.id]
        );
    }

    #[test]
    fn reorder_agents_rejects_unknown_id() {
        let (_tmp, project) = fresh_project();
        let a = project
            .register_agent("alpha", HarnessKind::ClaudeCode, None, None)
            .unwrap();
        let err = project.reorder_agents(&[Uuid::now_v7()]).unwrap_err();
        assert!(matches!(err, CoreError::ReorderRosterMismatch { .. }));
        assert_eq!(project.list_agents().unwrap()[0].id, a.id);
    }

    #[test]
    fn reorder_agents_rejects_duplicate_id() {
        let (_tmp, project) = fresh_project();
        let a = project
            .register_agent("alpha", HarnessKind::ClaudeCode, None, None)
            .unwrap();
        let b = project
            .register_agent("beta", HarnessKind::ClaudeCode, None, None)
            .unwrap();
        // Right length, but `b` appears twice and `a` never — a permutation
        // check by length alone would corrupt the registry here.
        let err = project.reorder_agents(&[b.id, b.id]).unwrap_err();
        assert!(matches!(err, CoreError::ReorderRosterMismatch { .. }));
        assert_eq!(
            project
                .list_agents()
                .unwrap()
                .iter()
                .map(|r| r.id)
                .collect::<Vec<_>>(),
            vec![a.id, b.id]
        );
    }

    #[test]
    fn set_session_locator_overwrites_an_existing_locator() {
        // Fork-and-heal shape: a locator already present is replaced.
        let (_tmp, project) = fresh_project();
        let a = project
            .register_agent("a", HarnessKind::Antigravity, None, None)
            .unwrap();
        let first = SessionLocator::Uuid(Uuid::new_v4());
        project.set_session_locator(a.id, first).unwrap();
        let healed = SessionLocator::Uuid(Uuid::new_v4());
        let updated = project.set_session_locator(a.id, healed.clone()).unwrap();
        assert_eq!(updated.session_locator, Some(healed.clone()));
        assert_eq!(
            project.list_agents().unwrap()[0].session_locator,
            Some(healed)
        );
    }

    #[test]
    fn set_session_locator_nonexistent_returns_not_found() {
        let (_tmp, project) = fresh_project();
        let err = project
            .set_session_locator(Uuid::now_v7(), SessionLocator::Uuid(Uuid::new_v4()))
            .unwrap_err();
        assert!(matches!(err, CoreError::AgentNotFound(_)));
    }

    #[test]
    fn set_session_locator_rejects_harness_shape_mismatch() {
        // A Codex locator on a Claude agent must be refused (it would never
        // resume) — and the registry left untouched.
        let (_tmp, project) = fresh_project();
        let claude = project
            .register_agent("c", HarnessKind::ClaudeCode, None, None)
            .unwrap();
        let before = project.list_agents().unwrap();
        let err = project
            .set_session_locator(
                claude.id,
                SessionLocator::Codex {
                    thread_id: "t".to_owned(),
                    partition_date: chrono::NaiveDate::from_ymd_opt(2026, 5, 16).unwrap(),
                },
            )
            .unwrap_err();
        assert!(matches!(
            err,
            CoreError::SessionLocatorHarnessMismatch { .. }
        ));
        assert_eq!(project.list_agents().unwrap(), before);

        // The inverse: a Uuid locator on a Codex agent is likewise refused.
        let codex = project
            .register_agent("x", HarnessKind::Codex, None, None)
            .unwrap();
        let err = project
            .set_session_locator(codex.id, SessionLocator::Uuid(Uuid::new_v4()))
            .unwrap_err();
        assert!(matches!(
            err,
            CoreError::SessionLocatorHarnessMismatch { .. }
        ));
    }

    #[test]
    fn register_rejects_invalid_name() {
        let (_tmp, project) = fresh_project();
        let err = project
            .register_agent("agent.1", HarnessKind::ClaudeCode, None, None)
            .unwrap_err();
        assert!(matches!(err, CoreError::InvalidName { .. }));
    }

    #[test]
    fn register_attached_claude_persists_provided_session_id() {
        let (_tmp, project) = fresh_project();
        let provided = Uuid::now_v7();
        let record = project
            .register_attached_claude_agent("attached", provided, None, None)
            .unwrap();
        assert_eq!(record.harness, HarnessKind::ClaudeCode);
        assert_eq!(record.session_locator, Some(SessionLocator::Uuid(provided)));
        // Round-trips via the registry.
        let listed = project.list_agents().unwrap();
        assert_eq!(listed, vec![record]);
    }

    #[test]
    fn register_agent_persists_model_and_effort_in_one_step() {
        let (_tmp, project) = fresh_project();
        let record = project
            .register_agent(
                "assistant",
                HarnessKind::ClaudeCode,
                Some("opus".to_owned()),
                Some("max".to_owned()),
            )
            .unwrap();
        assert_eq!(record.model.as_deref(), Some("opus"));
        assert_eq!(record.effort.as_deref(), Some("max"));
        // Durable: the values are on the appended record, not set by a
        // follow-up call.
        let listed = project.list_agents().unwrap();
        assert_eq!(listed, vec![record]);
    }

    #[test]
    fn profiles_round_trip_and_switch_atomically() {
        let (_tmp, project) = fresh_project();
        let agent = project
            .register_agent_with_profiles(
                "assistant",
                HarnessKind::ClaudeCode,
                Some("opus".to_owned()),
                Some("high".to_owned()),
                Some(AgentProfile {
                    model: Some("sonnet".to_owned()),
                    effort: Some("medium".to_owned()),
                }),
            )
            .unwrap();
        assert_eq!(agent.profiles.active, AgentProfileSlot::Primary);

        let switched = project
            .set_active_agent_profile(agent.id, AgentProfileSlot::Secondary)
            .unwrap();
        assert_eq!(switched.active_profile().model.as_deref(), Some("sonnet"));
        assert_eq!(
            project.list_agents().unwrap()[0].profiles.active,
            AgentProfileSlot::Secondary
        );

        let updated = project
            .set_agent_profiles(
                agent.id,
                AgentProfile {
                    model: Some("haiku".to_owned()),
                    effort: Some("low".to_owned()),
                },
                None,
            )
            .unwrap();
        assert_eq!(updated.model.as_deref(), Some("haiku"));
        assert_eq!(updated.effort.as_deref(), Some("low"));
        assert_eq!(updated.profiles, AgentProfiles::default());
    }

    #[test]
    fn switching_to_an_unconfigured_secondary_fails_without_mutating() {
        let (_tmp, project) = fresh_project();
        let agent = project
            .register_agent("assistant", HarnessKind::ClaudeCode, None, None)
            .unwrap();
        let before = project.list_agents().unwrap();

        let error = project
            .set_active_agent_profile(agent.id, AgentProfileSlot::Secondary)
            .unwrap_err();

        assert!(matches!(error, CoreError::SecondaryProfileMissing(id) if id == agent.id));
        assert_eq!(project.list_agents().unwrap(), before);
    }

    #[test]
    fn register_attached_claude_persists_model_and_effort() {
        let (_tmp, project) = fresh_project();
        let record = project
            .register_attached_claude_agent(
                "attached",
                Uuid::now_v7(),
                Some("sonnet".to_owned()),
                Some("low".to_owned()),
            )
            .unwrap();
        assert_eq!(record.model.as_deref(), Some("sonnet"));
        assert_eq!(record.effort.as_deref(), Some("low"));
        let listed = project.list_agents().unwrap();
        assert_eq!(listed, vec![record]);
    }

    #[test]
    fn register_attached_antigravity_carries_no_model_or_effort() {
        // Antigravity supports neither axis; both are structurally None.
        let (_tmp, project) = fresh_project();
        let record = project
            .register_attached_antigravity_agent("attached", Uuid::new_v4())
            .unwrap();
        assert_eq!(record.model, None);
        assert_eq!(record.effort, None);
    }

    /// An effort with no model is refused at **both** persistence sites and for
    /// **both** profile slots. Antigravity's valid levels are a property of the
    /// chosen model, so an effort alone is not dispatchable — and storing it
    /// would leave a record asserting a selection no turn can apply.
    #[test]
    fn effort_without_a_model_is_refused_at_registration() {
        let (_tmp, project) = fresh_project();
        let err = project
            .register_agent("a", HarnessKind::Antigravity, None, Some("high".to_owned()))
            .unwrap_err();
        assert!(
            matches!(
                err,
                CoreError::EffortWithoutModel {
                    harness: HarnessKind::Antigravity
                }
            ),
            "{err:?}"
        );
        assert!(
            project.list_agents().unwrap().is_empty(),
            "rejected before the append — no orphan record"
        );
    }

    #[test]
    fn effort_without_a_model_is_refused_when_setting_profiles() {
        let (_tmp, project) = fresh_project();
        let agent = project
            .register_agent("a", HarnessKind::Antigravity, None, None)
            .unwrap();

        // Primary slot.
        let err = project
            .set_agent_profiles(
                agent.id,
                AgentProfile {
                    model: None,
                    effort: Some("high".to_owned()),
                },
                None,
            )
            .unwrap_err();
        assert!(
            matches!(err, CoreError::EffortWithoutModel { .. }),
            "{err:?}"
        );

        // Secondary slot — the same rule, not just the primary one.
        let err = project
            .set_agent_profiles(
                agent.id,
                AgentProfile {
                    model: Some("gemini-3.1-pro".to_owned()),
                    effort: Some("high".to_owned()),
                },
                Some(AgentProfile {
                    model: None,
                    effort: Some("low".to_owned()),
                }),
            )
            .unwrap_err();
        assert!(
            matches!(err, CoreError::EffortWithoutModel { .. }),
            "{err:?}"
        );
    }

    /// The regression this capability gate exists to prevent. Claude and Codex
    /// emit their effort flag independently of the model, so "harness's own
    /// default model, at an explicit effort" is a valid, dispatchable profile —
    /// and one the editor actively produces when a user sets an effort and then
    /// returns the model picker to "Default". An unconditional
    /// effort-requires-model rule rejected it on save.
    #[test]
    fn effort_without_a_model_stays_valid_where_the_harness_allows_it() {
        for harness in [HarnessKind::ClaudeCode, HarnessKind::Codex] {
            let (_tmp, project) = fresh_project();

            // At registration.
            let record = project
                .register_agent("a", harness, None, Some("high".to_owned()))
                .unwrap_or_else(|e| panic!("{harness:?} registration: {e:?}"));
            assert_eq!(record.model, None);
            assert_eq!(record.effort.as_deref(), Some("high"));

            // And when editing profiles afterwards, in both slots.
            project
                .set_agent_profiles(
                    record.id,
                    AgentProfile {
                        model: None,
                        effort: Some("high".to_owned()),
                    },
                    Some(AgentProfile {
                        model: None,
                        effort: Some("low".to_owned()),
                    }),
                )
                .unwrap_or_else(|e| panic!("{harness:?} set_agent_profiles: {e:?}"));
        }
    }

    #[test]
    fn antigravity_now_persists_a_model_and_effort() {
        // The capability that `agy` 1.1.x opened: `--model` dispatches
        // headlessly without touching the harness's own global config, so the
        // selection is ours to store.
        let (_tmp, project) = fresh_project();
        let record = project
            .register_agent(
                "a",
                HarnessKind::Antigravity,
                Some("gemini-3.1-pro".to_owned()),
                Some("high".to_owned()),
            )
            .unwrap();
        assert_eq!(record.model.as_deref(), Some("gemini-3.1-pro"));
        assert_eq!(record.effort.as_deref(), Some("high"));
    }

    /// A model with no effort is **not** refused here. Which models require one
    /// is a catalog fact, and duplicating that catalog into core would give it
    /// two sources of truth; `agy` rejects the combination pre-dispatch,
    /// quota-free, naming the valid levels.
    #[test]
    fn a_model_without_an_effort_is_left_to_the_harness_to_judge() {
        let (_tmp, project) = fresh_project();
        let record = project
            .register_agent(
                "a",
                HarnessKind::Antigravity,
                Some("gemini-3.1-pro".to_owned()),
                None,
            )
            .unwrap();
        assert_eq!(record.effort, None);
    }

    #[test]
    fn core_normalizes_blank_selection_regardless_of_caller() {
        // The persistence boundary, not just the IPC layer, drops a blank
        // selection — so a direct-core caller can't persist a dispatch-breaking
        // `Some("")` either.
        let (_tmp, project) = fresh_project();
        let agent = project
            .register_agent(
                "a",
                HarnessKind::ClaudeCode,
                Some("  ".to_owned()),
                Some(String::new()),
            )
            .unwrap();
        assert_eq!(agent.model, None);
        assert_eq!(agent.effort, None);

        project
            .set_agent_profiles(
                agent.id,
                AgentProfile {
                    model: Some("   ".to_owned()),
                    effort: Some(" ".to_owned()),
                },
                None,
            )
            .unwrap();
        let reloaded = &project.list_agents().unwrap()[0];
        assert_eq!(reloaded.model, None);
        assert_eq!(reloaded.effort, None);
    }

    #[test]
    fn register_attached_codex_persists_thread_id_and_date() {
        let (_tmp, project) = fresh_project();
        let date = chrono::NaiveDate::from_ymd_opt(2026, 5, 16).unwrap();
        let record = project
            .register_attached_codex_agent("attached", "thread-abc".to_owned(), date, None, None)
            .unwrap();
        assert_eq!(record.harness, HarnessKind::Codex);
        assert_eq!(
            record.session_locator,
            Some(SessionLocator::Codex {
                thread_id: "thread-abc".to_owned(),
                partition_date: date,
            })
        );
    }

    #[test]
    fn register_attached_antigravity_persists_conversation_uuid() {
        let (_tmp, project) = fresh_project();
        let conversation_id = Uuid::new_v4();
        let record = project
            .register_attached_antigravity_agent("attached", conversation_id)
            .unwrap();
        assert_eq!(record.harness, HarnessKind::Antigravity);
        assert_eq!(
            record.session_locator,
            Some(SessionLocator::Uuid(conversation_id))
        );
    }

    #[test]
    fn register_attached_enforces_name_uniqueness_across_create_and_attach() {
        let (_tmp, project) = fresh_project();
        project
            .register_agent("agent-a", HarnessKind::ClaudeCode, None, None)
            .unwrap();
        let err = project
            .register_attached_claude_agent("agent_a", Uuid::now_v7(), None, None)
            .unwrap_err();
        assert!(matches!(err, CoreError::DuplicateAgentName { .. }));
    }

    #[test]
    fn register_attached_validates_name() {
        let (_tmp, project) = fresh_project();
        let err = project
            .register_attached_claude_agent("bad.name", Uuid::now_v7(), None, None)
            .unwrap_err();
        assert!(matches!(err, CoreError::InvalidName { .. }));
    }

    #[test]
    fn unsupported_config_version_surfaces_typed_error() {
        let (_tmp, project) = fresh_project();
        // Write a bad version to the project's config.yaml.
        std::fs::write(
            project.root.join(CONFIG_FILE),
            "version: 99\nname: x\ncreated_at: 2026-05-12T00:00:00Z\n",
        )
        .unwrap();
        let err = load(&project.directory, project.id, project.root.clone()).unwrap_err();
        assert!(matches!(
            err,
            CoreError::UnsupportedConfigVersion {
                found: 99,
                expected: 1,
                ..
            }
        ));
    }

    #[test]
    fn corrupt_registry_line_surfaces_typed_error_with_line_number() {
        let (_tmp, project) = fresh_project();
        // Append a valid record then a malformed line.
        project
            .register_agent("assistant", HarnessKind::ClaudeCode, None, None)
            .unwrap();
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&project.registry_path)
            .unwrap();
        writeln!(f, "this is not json").unwrap();

        let err = project.list_agents().unwrap_err();
        match err {
            CoreError::CorruptJsonl {
                line_number, line, ..
            } => {
                assert_eq!(line_number, 2);
                assert_eq!(line, "this is not json");
            }
            other => panic!("expected CorruptJsonl, got {other:?}"),
        }
    }
}

#[cfg(test)]
mod adopt_tests {
    use super::*;
    use tempfile::TempDir;

    fn fresh_project(name: &str) -> (TempDir, Project) {
        let tmp = TempDir::new().unwrap();
        let projects_dir = tmp.path().join("projects");
        create_dir_all(&projects_dir).unwrap();
        let (_summary, project) = create_on_disk(tmp.path(), None, &projects_dir, name).unwrap();
        (tmp, project)
    }

    fn two_projects() -> (TempDir, Project, Project) {
        let tmp = TempDir::new().unwrap();
        let projects_dir = tmp.path().join("projects");
        create_dir_all(&projects_dir).unwrap();
        let (_s1, source) = create_on_disk(tmp.path(), None, &projects_dir, "source").unwrap();
        let (_s2, target) = create_on_disk(tmp.path(), None, &projects_dir, "target").unwrap();
        (tmp, source, target)
    }

    #[test]
    fn a_fork_records_where_its_parents_transcript_lives() {
        // For an unmoved parent that is the shared project directory; the value
        // is captured at creation because it cannot be safely re-derived at
        // dispatch time once projects can move.
        let (_tmp, project) = fresh_project("forking");
        let parent = project
            .register_agent("alice", HarnessKind::ClaudeCode, None, None)
            .unwrap();

        let child = project.fork_agent(parent.id).unwrap();

        assert_eq!(
            child.forked_from_session_home.as_deref(),
            Some(project.directory.as_path())
        );
    }

    #[test]
    fn a_fork_of_a_moved_parent_records_the_parents_recorded_home() {
        // The "move an agent, then branch it" sequence: the parent's transcript
        // stays under the directory the move recorded, and the fork must lock
        // and resume it there — not under the project they now share.
        let (_tmp, source, target) = two_projects();
        let parent = source
            .register_agent("alice", HarnessKind::ClaudeCode, None, None)
            .unwrap();
        let home = PathBuf::from("/repos/original-checkout");
        let moved = target.adopt_agent(&parent, Some(home.clone())).unwrap();
        assert_eq!(moved.session_home, Some(home.clone()));

        let child = target.fork_agent(moved.id).unwrap();

        assert_eq!(child.forked_from_session_home, Some(home));
        assert_eq!(
            child.session_home, None,
            "the child itself is native to the project it was forked in"
        );
    }

    #[test]
    fn a_registry_line_with_provenance_but_no_parent_fails_the_load() {
        use std::io::Write;

        let (_tmp, project) = fresh_project("corrupt");
        let line = serde_json::json!({
            "id": Uuid::now_v7(),
            "project_id": project.id,
            "name": "orphan",
            "harness": "claude_code",
            "session_locator": {"uuid": Uuid::now_v7()},
            "forked_from_session_home": "/repos/somewhere",
            "created_at": "2026-05-14T04:43:19Z",
        });
        let mut file = std::fs::File::create(&project.registry_path).unwrap();
        writeln!(file, "{line}").unwrap();

        let err = project.list_agents().unwrap_err();

        assert!(
            matches!(err, CoreError::ForkProvenanceWithoutParent(_)),
            "got {err:?}"
        );
    }

    #[test]
    fn adopt_restamps_project_id_and_records_session_home() {
        let (_tmp, source, target) = two_projects();
        let record = source
            .register_agent("mover", HarnessKind::ClaudeCode, None, None)
            .unwrap();
        let home = PathBuf::from("/repos/checkout");

        let adopted = target.adopt_agent(&record, Some(home.clone())).unwrap();

        assert_eq!(adopted.id, record.id, "identity survives a move");
        assert_eq!(adopted.session_locator, record.session_locator);
        assert_eq!(adopted.project_id, target.id);
        assert_eq!(adopted.session_home, Some(home));
        assert_eq!(target.list_agents().unwrap(), vec![adopted]);
    }

    #[test]
    fn adopt_without_a_session_home_leaves_the_field_unset() {
        // A same-directory move: the agent's sessions still live under the
        // project's own working directory, so stamping a home would be a lie.
        let (_tmp, source, target) = two_projects();
        let record = source
            .register_agent("mover", HarnessKind::ClaudeCode, None, None)
            .unwrap();

        let adopted = target.adopt_agent(&record, None).unwrap();

        assert_eq!(adopted.session_home, None);
    }

    #[test]
    fn a_second_move_preserves_the_home_recorded_by_the_first() {
        // The transcript never leaves the directory it was first encoded from,
        // so the first answer stays the right one however many moves follow.
        let (_tmp, source, target) = two_projects();
        let original = source
            .register_agent("mover", HarnessKind::ClaudeCode, None, None)
            .unwrap();
        let first_home = PathBuf::from("/repos/original-checkout");
        let once = target
            .adopt_agent(&original, Some(first_home.clone()))
            .unwrap();

        let (_tmp2, third) = fresh_project("third");
        let twice = third.adopt_agent(&once, None).unwrap();

        assert_eq!(twice.session_home, Some(first_home));
        assert_eq!(twice.project_id, third.id);
    }

    #[test]
    fn adopting_an_id_already_present_is_a_no_op() {
        // Move recovery re-drives every step; the append must not duplicate.
        let (_tmp, source, target) = two_projects();
        let record = source
            .register_agent("mover", HarnessKind::ClaudeCode, None, None)
            .unwrap();
        let home = PathBuf::from("/repos/checkout");
        let first = target.adopt_agent(&record, Some(home.clone())).unwrap();

        let second = target.adopt_agent(&record, Some(home)).unwrap();

        assert_eq!(first, second);
        assert_eq!(target.list_agents().unwrap().len(), 1);
    }

    #[test]
    fn adopt_refuses_a_canonically_colliding_name_and_writes_nothing() {
        // Names collide case-insensitively with hyphens as underscores, so a
        // move must refuse exactly where register/rename would.
        let (_tmp, source, target) = two_projects();
        let resident = target
            .register_agent("Agent-A", HarnessKind::ClaudeCode, None, None)
            .unwrap();
        let incoming = source
            .register_agent("agent_a", HarnessKind::ClaudeCode, None, None)
            .unwrap();

        let err = target.adopt_agent(&incoming, None).unwrap_err();

        assert!(
            matches!(err, CoreError::DuplicateAgentName { .. }),
            "expected a duplicate-name refusal, got {err:?}"
        );
        assert_eq!(target.list_agents().unwrap(), vec![resident]);
        assert_eq!(
            source.list_agents().unwrap(),
            vec![incoming],
            "a refused adoption leaves the source untouched"
        );
    }

    #[test]
    fn a_second_move_proposing_the_identical_home_succeeds() {
        // The recompute-on-re-drive shape: recovery proposes the same value it
        // proposed before, which must be accepted rather than read as a
        // contradiction.
        let (_tmp, source, target) = two_projects();
        let record = source
            .register_agent("mover", HarnessKind::ClaudeCode, None, None)
            .unwrap();
        let home = PathBuf::from("/repos/original-checkout");
        let once = target.adopt_agent(&record, Some(home.clone())).unwrap();

        let (_tmp2, third) = fresh_project("third");
        let twice = third.adopt_agent(&once, Some(home.clone())).unwrap();

        assert_eq!(twice.session_home, Some(home));
        assert_eq!(twice.project_id, third.id);
    }

    #[test]
    fn a_second_move_proposing_a_different_home_is_refused() {
        // The recorded home is immutable historical identity — the transcript
        // stays where it was first encoded. A caller computing something else
        // has a bug, and silently preferring the recorded value would hide it.
        let (_tmp, source, target) = two_projects();
        let record = source
            .register_agent("mover", HarnessKind::ClaudeCode, None, None)
            .unwrap();
        let original_home = PathBuf::from("/repos/original-checkout");
        let once = target
            .adopt_agent(&record, Some(original_home.clone()))
            .unwrap();

        let (_tmp2, third) = fresh_project("third");
        let err = third
            .adopt_agent(&once, Some(PathBuf::from("/repos/somewhere-else")))
            .unwrap_err();

        assert!(
            matches!(
                &err,
                CoreError::SessionHomeContradiction { agent_id, recorded, .. }
                    if *agent_id == record.id && *recorded == original_home
            ),
            "expected a session-home contradiction, got {err:?}"
        );
        assert!(third.list_agents().unwrap().is_empty());
        assert_eq!(
            target.list_agents().unwrap(),
            vec![once],
            "the recorded home is left exactly as it was"
        );
    }

    #[test]
    fn a_target_row_differing_only_in_session_home_is_a_conflict() {
        // Guards the full-record equality rule on the one field whose exclusion
        // would sound reasonable ("it is expected to change across moves") and
        // be wrong: a divergent home is precisely the divergence worth catching.
        let (_tmp, source, target) = two_projects();
        let record = source
            .register_agent("mover", HarnessKind::ClaudeCode, None, None)
            .unwrap();
        let seated = target
            .adopt_agent(&record, Some(PathBuf::from("/repos/checkout-x")))
            .unwrap();

        let err = target
            .adopt_agent(&record, Some(PathBuf::from("/repos/checkout-y")))
            .unwrap_err();

        assert!(
            matches!(err, CoreError::AgentAdoptionConflict { agent_id } if agent_id == record.id),
            "expected an adoption conflict, got {err:?}"
        );
        assert_eq!(target.list_agents().unwrap(), vec![seated]);
    }

    #[test]
    fn adopting_a_conflicting_row_under_the_same_id_is_refused() {
        // Recovery re-drives adoption, so a matching row means "already done".
        // A *differing* row under the same id means something else wrote it —
        // accepting it would report a finished move and let the caller delete
        // the source copy.
        let (_tmp, source, target) = two_projects();
        let record = source
            .register_agent("mover", HarnessKind::ClaudeCode, None, None)
            .unwrap();
        let impostor = AgentRecord {
            session_locator: Some(SessionLocator::Uuid(Uuid::now_v7())),
            ..record.clone()
        };
        let seated = target.adopt_agent(&impostor, None).unwrap();

        let err = target.adopt_agent(&record, None).unwrap_err();

        assert!(
            matches!(err, CoreError::AgentAdoptionConflict { agent_id } if agent_id == record.id),
            "expected an adoption conflict, got {err:?}"
        );
        assert_eq!(
            target.list_agents().unwrap(),
            vec![seated],
            "the conflicting row is left exactly as it was"
        );
        assert_eq!(source.list_agents().unwrap(), vec![record]);
    }

    #[test]
    fn adopt_refuses_a_session_home_on_a_harness_that_does_not_use_one() {
        // Only Claude namespaces session storage by working directory, so a
        // home on any other harness is inert data that would later acquire
        // accidental meaning.
        let (_tmp, source, target) = two_projects();
        let codex = source
            .register_agent("coder", HarnessKind::Codex, None, None)
            .unwrap();

        let err = target
            .adopt_agent(&codex, Some(PathBuf::from("/repos/checkout")))
            .unwrap_err();

        assert!(
            matches!(err, CoreError::SessionHomeUnsupported { harness } if harness == HarnessKind::Codex),
            "expected a session-home refusal, got {err:?}"
        );
        assert!(target.list_agents().unwrap().is_empty());
    }

    #[test]
    fn a_registry_line_with_a_session_home_on_a_non_claude_harness_fails_the_load() {
        use std::io::Write;

        let (_tmp, project) = fresh_project("contradictory");
        let line = serde_json::json!({
            "id": Uuid::now_v7(),
            "project_id": project.id,
            "name": "coder",
            "harness": "codex",
            "session_locator": null,
            "session_home": "/repos/checkout",
            "created_at": "2026-05-14T04:43:19Z",
        });
        let mut file = std::fs::File::create(&project.registry_path).unwrap();
        writeln!(file, "{line}").unwrap();

        let err = project.list_agents().unwrap_err();

        assert!(
            matches!(err, CoreError::SessionHomeUnsupported { harness } if harness == HarnessKind::Codex),
            "expected a session-home refusal, got {err:?}"
        );
    }

    #[test]
    fn a_record_carrying_session_home_round_trips_through_the_registry() {
        let (_tmp, source, target) = two_projects();
        let record = source
            .register_agent("mover", HarnessKind::ClaudeCode, None, None)
            .unwrap();
        let home = PathBuf::from("/repos/checkout with spaces");
        let adopted = target.adopt_agent(&record, Some(home.clone())).unwrap();

        let reread = target.list_agents().unwrap();

        assert_eq!(reread, vec![adopted]);
        assert_eq!(reread[0].session_home, Some(home));
    }

    #[test]
    fn a_registry_line_without_session_home_loads_as_none() {
        // Backward compatibility: every record written before moves existed
        // lacks the key and must load as "sessions live under the project".
        use std::io::Write;

        let (_tmp, project) = fresh_project("legacy");
        let line = serde_json::json!({
            "id": Uuid::now_v7(),
            "project_id": project.id,
            "name": "legacy",
            "harness": "claude_code",
            "session_locator": {"uuid": Uuid::now_v7()},
            "model": null,
            "effort": null,
            "forked_from_session": null,
            "created_at": "2026-05-14T04:43:19Z",
        });
        let mut file = std::fs::File::create(&project.registry_path).unwrap();
        writeln!(file, "{line}").unwrap();

        let agents = project.list_agents().unwrap();

        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].session_home, None);
    }
}
