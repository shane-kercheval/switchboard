import type { AgentId, AgentRecord } from "./types";
import {
  type NameValidation,
  canonicalizeForUniqueness,
  normalizeName,
  validateName,
} from "./nameValidation";

/// Agent-name validation — the agent-scoped wrapper over the shared
/// `nameValidation` rules (mirrors `crates/core/src/name.rs`). The format and
/// canonicalization rules live in the shared module so agent and project names
/// can never drift from each other or from the backend.

export { canonicalizeForUniqueness, type NameValidation };

/// See `nameValidation.normalizeName`. Kept under this name for the agent
/// call sites (create form, sidebar rename) that import it.
export function normalizeAgentName(name: string): string {
  return normalizeName(name);
}

/// Validate a candidate agent name against the format rules and the live
/// roster. The candidate is trimmed first, matching what the create/rename
/// flows submit. `excludeAgentId` drops one agent from the uniqueness check —
/// pass the agent being renamed so re-saving its own (unchanged, or
/// case/hyphen-variant) name is not flagged as a self-collision. Omit when
/// creating.
export function validateAgentName(
  candidate: string,
  roster: AgentRecord[],
  excludeAgentId?: AgentId,
): NameValidation {
  return validateName(candidate, roster, {
    excludeId: excludeAgentId,
    duplicateMessage: (name) => `An agent named '${name}' already exists`,
  });
}
