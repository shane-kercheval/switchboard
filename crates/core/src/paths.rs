//! Shared filename / directory-name constants for the `.switchboard/`
//! layout. Centralized here so a future schema rename only has to touch
//! one place — and to prevent `directory.rs` and `project.rs` from
//! diverging on the exact spellings (which they did before consolidation).

pub(crate) const SWITCHBOARD_DIR: &str = ".switchboard";
pub(crate) const CONFIG_FILE: &str = "config.yaml";
pub(crate) const REGISTRY_FILE: &str = "registry.jsonl";
pub(crate) const PROJECTS_INDEX: &str = "projects.jsonl";
pub(crate) const PROJECTS_DIR: &str = "projects";
pub(crate) const JOURNAL_FILE: &str = "journal.jsonl";
pub(crate) const PINS_FILE: &str = "pins.jsonl";
pub(crate) const ATTACHMENTS_DIR: &str = "attachments";
/// User-global store only (see `store.rs`): the working-directory catalog and
/// the store's own schema marker. Deliberately *not* `config.yaml` — the store
/// root and a project root are different scopes, and reusing the name would make
/// a mis-joined path silently parse as the wrong thing.
pub(crate) const DIRECTORIES_CATALOG: &str = "directories.jsonl";
pub(crate) const STORE_CONFIG_FILE: &str = "store.yaml";
pub(crate) const RUNS_DIR: &str = "runs";
