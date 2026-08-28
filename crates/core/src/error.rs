use std::path::PathBuf;

pub type Result<T> = std::result::Result<T, CoreError>;

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CoreError {
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("path is not a directory: {path}")]
    NotADirectory { path: PathBuf },

    #[error("invalid name {name:?}: must match `^[A-Za-z0-9_-]+$` and be non-empty")]
    InvalidName { name: String },

    #[error("agent name {name:?} already exists in this project (collides with {existing:?})")]
    DuplicateAgentName { name: String, existing: String },

    #[error("project name {name:?} already exists in this directory (collides with {existing:?})")]
    DuplicateProjectName { name: String, existing: String },

    #[error("project not found: {0}")]
    ProjectNotFound(uuid::Uuid),

    /// A `directory_id` referenced by a project index entry has no catalog
    /// entry. The catalog never deletes an entry a project still references, so
    /// this is corruption (a hand-edited or truncated `directories.jsonl`), not
    /// an ordinary "the user removed that directory" state — a removed working
    /// *directory* keeps its catalog entry with a path that no longer resolves.
    #[error("directory not found in the store catalog: {0}")]
    DirectoryNotFound(uuid::Uuid),

    #[error("agent not found: {0}")]
    AgentNotFound(uuid::Uuid),

    #[error("unsupported config version at {path}: found {found}, expected {expected}")]
    UnsupportedConfigVersion {
        path: PathBuf,
        found: u32,
        expected: u32,
    },

    #[error("corrupt JSONL at {path} (line {line_number}): {source}\n  line: {line}")]
    CorruptJsonl {
        path: PathBuf,
        line_number: usize,
        line: String,
        #[source]
        source: serde_json::Error,
    },

    #[error("corrupt YAML at {path}: {source}")]
    CorruptYaml {
        path: PathBuf,
        #[source]
        source: serde_norway::Error,
    },

    /// `edit_yaml_mapping` was asked to edit a file that parses to something other
    /// than a top-level mapping. Refused rather than clobbered — a file with real
    /// content we can't safely round-trip (e.g. a hand-edited shared config) must
    /// not be silently overwritten.
    #[error("{path} is not a YAML mapping; refusing to overwrite it")]
    NotAMapping { path: PathBuf },

    #[error("failed to serialize value for {path}: {source}")]
    Serialize {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error("expected append-only file is missing after init: {path}")]
    MissingAppendOnlyFile { path: PathBuf },

    #[error(
        "session locator shape does not match agent {agent_id}'s harness {harness} \
         — refusing to persist a locator that would not resume"
    )]
    SessionLocatorHarnessMismatch {
        agent_id: uuid::Uuid,
        harness: crate::harness::HarnessKind,
    },

    #[error(
        "{harness} does not support {axis} selection \
         — refusing to persist a selection it can never apply"
    )]
    SelectionUnsupported {
        harness: crate::harness::HarnessKind,
        axis: crate::harness::SelectionAxis,
    },

    /// An effort was selected with no model to apply it to.
    ///
    /// Distinct from [`Self::SelectionUnsupported`], which is a statement about
    /// the *harness*: this one is about the *profile* being incoherent whatever
    /// the harness supports. It exists for Antigravity, whose valid effort
    /// levels are a property of the chosen model — `agy` rejects an effort that
    /// the model does not offer, and offers none at all without a model — so an
    /// effort alone cannot be dispatched or even validated. Refused at the
    /// persistence boundary rather than dropped at dispatch, which would leave
    /// a stored profile claiming something the turn never did.
    #[error(
        "{harness} cannot apply a reasoning effort without a model          — the valid levels depend on which model is selected"
    )]
    EffortWithoutModel {
        harness: crate::harness::HarnessKind,
    },

    #[error("agent {0} cannot activate a secondary profile because none is configured")]
    SecondaryProfileMissing(crate::agent::AgentId),

    /// Deliberately a statement about **Switchboard's support**, not about what
    /// the harness's CLI can do — Codex, for one, can branch a session (through
    /// an integration Switchboard doesn't wire). Claiming otherwise would be
    /// false, and this message is the source for the user-facing explanation on
    /// non-forkable agents.
    #[error(
        "Switchboard does not support forking {harness} sessions \
         — refusing to record a branch it could never materialize"
    )]
    SessionForkUnsupported {
        harness: crate::harness::HarnessKind,
    },

    /// Fork provenance forms a loop: following `forked_from_session` from this
    /// agent leads back to it. Unreachable through any supported path — a fork's
    /// parent always predates it — so this means the registry was edited or
    /// corrupted.
    ///
    /// Rejected at **load**, not merely ignored, because a cycle is not
    /// cosmetic: the materializing-fork gate asks each agent's own actor whether
    /// its parent is mid-turn, so a loop makes two actors wait on each other's
    /// reply and neither can answer. The single-agent case is caught earlier, at
    /// the gate; longer loops can only be caught here, where the whole set is
    /// visible at once.
    #[error(
        "agent {agent_id}'s fork provenance forms a cycle — the registry is \
         inconsistent and cannot be loaded safely"
    )]
    ForkProvenanceCycle { agent_id: uuid::Uuid },

    /// Two records in one registry share an identity that must be unique. Fatal
    /// on read rather than tolerated: duplicate ids collapse in the app's
    /// agent cache (two roster rows sharing one runtime and one actor), and
    /// duplicate session locators mean two agents driving one harness
    /// conversation — and they silently corrupt the provenance walk, whose
    /// session-keyed map keeps only one of the pair.
    #[error(
        "{registry}: agents {first} and {second} share the same {field} — \
         the registry is inconsistent and cannot be loaded safely. Remove or \
         correct one of the two records to reopen this project."
    )]
    DuplicateAgentIdentity {
        registry: std::path::PathBuf,
        field: &'static str,
        first: uuid::Uuid,
        second: uuid::Uuid,
    },

    /// A record sitting in one project's registry claims to belong to another.
    /// Fatal because it is a routing invariant, not a label: dispatch resolves an
    /// agent's project — and therefore its working directory and journal — from
    /// this field, so a mismatched record silently runs the agent's work against
    /// a different project's directory whenever that project is also loaded.
    #[error(
        "{registry}: agent {agent_id} claims project {claimed} but is stored under \
         {actual} — the registry is inconsistent and cannot be loaded safely"
    )]
    AgentProjectMismatch {
        registry: std::path::PathBuf,
        agent_id: uuid::Uuid,
        claimed: uuid::Uuid,
        actual: uuid::Uuid,
    },

    /// The source agent carries no session id to branch from. Distinct from
    /// [`Self::SessionForkUnsupported`]: that harness can never fork; this one
    /// could, but this record has nothing to fork *from*.
    ///
    /// Currently unreachable through any supported path — every fork-capable
    /// harness pre-generates its locator at registration, so a valid record
    /// always has one and reaching this means the registry is inconsistent.
    /// That is a property of today's harness set, **not** of the state itself:
    /// a fork-capable harness that captured its locator at runtime would make
    /// this a genuine "not yet — dispatch it once first," and the message below
    /// already reads correctly for that case.
    #[error("agent {agent_id} has no session to branch from")]
    SessionForkSourceMissing { agent_id: uuid::Uuid },

    /// A reorder's id list must be an exact permutation of the current roster.
    /// Covers every shape failure (wrong length, unknown id, duplicate id) with
    /// one variant: the caller's list is stale or malformed either way, and the
    /// remedy is identical — re-read the roster and retry.
    #[error(
        "reorder id list must contain each current agent id exactly once \
         ({provided} ids provided for {expected} agents)"
    )]
    ReorderRosterMismatch { expected: usize, provided: usize },
}

impl CoreError {
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}
