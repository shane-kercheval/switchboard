//! Stable identity types.
//!
//! Both are `Uuid` aliases, and both live here rather than beside the types
//! that own them: `project` is the lower module and `store` builds on it, so
//! defining `DirectoryId` in `store` and naming it from `project` would point
//! the dependency arrow both ways for two type aliases.

use uuid::Uuid;

/// Stable identity for a project. Minted at creation and never derived from the
/// name, which the user can change.
pub type ProjectId = Uuid;

/// Stable identity for a working directory, minted on first registration.
///
/// **Not derived from the path**, which is the entire point: the path is
/// mutable state (a repo gets moved, a worktree gets recreated elsewhere) and
/// the id is what projects reference, so re-pointing is a one-line catalog
/// rewrite instead of a rewrite of every project entry.
pub type DirectoryId = Uuid;
