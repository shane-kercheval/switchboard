//! Shared filename / directory-name constants for the user-global store
//! layout. Centralized here so a future schema rename only has to touch one
//! place — and to prevent `store.rs` and `project.rs` from diverging on the
//! exact spellings (which they did before consolidation).
//!
//! `.switchboard` is deliberately absent: nothing writes a per-directory layout
//! any more, and keeping the constant would invite a read path back.

pub(crate) const CONFIG_FILE: &str = "config.yaml";
pub(crate) const REGISTRY_FILE: &str = "registry.jsonl";
pub(crate) const PROJECTS_INDEX: &str = "projects.jsonl";
pub(crate) const PROJECTS_DIR: &str = "projects";
pub(crate) const JOURNAL_FILE: &str = "journal.jsonl";
pub(crate) const PINS_FILE: &str = "pins.jsonl";
pub(crate) const ATTACHMENTS_DIR: &str = "attachments";
/// The version-1 directory catalog, read only by the store's in-place
/// migration, and the name it is set aside under afterwards.
pub(crate) const DIRECTORIES_CATALOG_V1: &str = "directories.jsonl";
pub(crate) const DIRECTORIES_CATALOG_V1_BACKUP: &str = "directories.jsonl.v1.bak";
/// The store's own schema marker. Deliberately *not* `config.yaml` — the store
/// root and a project root are different scopes, and reusing the name would make
/// a mis-joined path silently parse as the wrong thing.
pub(crate) const STORE_CONFIG_FILE: &str = "store.yaml";
pub(crate) const RUNS_DIR: &str = "runs";
