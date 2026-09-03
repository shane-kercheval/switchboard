//! Stable identity types.

use uuid::Uuid;

/// Stable identity for a project. Minted at creation and never derived from the
/// name, which the user can change.
pub type ProjectId = Uuid;
