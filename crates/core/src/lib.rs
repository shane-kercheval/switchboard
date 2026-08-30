//! Switchboard core — pure-Rust persistence and registry types. No Tauri, no async.
//!
//! The user-global store (`<config-dir>/store/`) is the single source of truth
//! for what projects exist and what agents live in them. **Switchboard writes
//! nothing into a working directory** — a directory is referenced by a stable
//! `directory_id` the store's catalog maps to a path, so deleting or moving a
//! checkout costs no project state. See `docs/system-design.md` §3 for the
//! canonical spec.

pub mod agent;
pub mod attachment;
pub mod directory;
pub mod error;
pub mod harness;
pub mod ids;
mod io;
pub mod journal;
pub mod name;
mod paths;
pub mod pins;
pub mod project;
pub mod store;

pub use agent::{
    AgentId, AgentProfile, AgentProfileSlot, AgentProfiles, AgentRecord, SessionLocator,
    normalize_selection,
};
pub use attachment::{
    Attachment, AttachmentKind, render_dispatched_prompt_with_attachments,
    render_prompt_with_attachments,
};
pub use directory::Directory;
pub use error::{CoreError, Result};
pub use harness::{HarnessKind, SelectionAxis};
pub use ids::{DirectoryId, ProjectId};
pub use io::{append_jsonl, edit_yaml_mapping, read_jsonl, read_yaml, write_yaml};
pub use journal::{JournalRecord, SendId};
pub use pins::MessagePin;
pub use project::{Project, ProjectConfig, ProjectSummary};
pub use store::{
    DirectoryEntry, DirectoryResolution, ProjectEntry, ResolvedProject, STORE_VERSION, Store,
    StoreConfig,
};
