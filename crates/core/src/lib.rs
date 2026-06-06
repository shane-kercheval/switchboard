//! Switchboard core — pure-Rust persistence and registry types. No Tauri, no async.
//!
//! The on-disk layout under `<directory>/.switchboard/` is the single source of
//! truth for what projects exist and what agents live in them. See
//! `docs/system-design.md` §3 for the canonical spec.

pub mod agent;
pub mod directory;
pub mod error;
pub mod harness;
mod io;
pub mod journal;
pub mod name;
mod paths;
pub mod project;

pub use agent::{AgentId, AgentRecord, SessionLocator};
pub use directory::{Directory, DirectoryConfig};
pub use error::{CoreError, Result};
pub use harness::HarnessKind;
pub use io::{edit_yaml_mapping, read_yaml, write_yaml};
pub use journal::{JournalRecord, SendId};
pub use project::{Project, ProjectConfig, ProjectId, ProjectSummary};
