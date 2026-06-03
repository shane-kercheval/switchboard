import type { ProjectId } from "./types";
import { type NameValidation, normalizeName, validateName } from "./nameValidation";

/// Project-name validation — the project-scoped wrapper over the shared
/// `nameValidation` rules. Project names are unique **per directory** (mirrors
/// `Directory::rename_project` / `create_project` in core), so the caller passes
/// the sibling set already filtered to the same directory.

/// See `nameValidation.normalizeName`. Kept under this name for the project
/// rename/create call sites.
export function normalizeProjectName(name: string): string {
  return normalizeName(name);
}

/// Validate a candidate project name against the format rules and `siblings`
/// (the *other* projects in the same directory — the caller supplies the set
/// scoped to one directory). Accepts the minimal `{id, name}` shape so both the
/// rename path (`ProjectListing[]`) and the create path (`ProjectSummary[]` from
/// the folder probe) can call it. `excludeProjectId` drops the project being
/// renamed from the uniqueness check so re-saving its own (or a
/// case/hyphen-variant) name is not a self-collision.
export function validateProjectName(
  candidate: string,
  siblings: ReadonlyArray<{ id: string; name: string }>,
  excludeProjectId?: ProjectId,
): NameValidation {
  return validateName(candidate, siblings, {
    excludeId: excludeProjectId,
    duplicateMessage: (name) => `A project named '${name}' already exists`,
  });
}
