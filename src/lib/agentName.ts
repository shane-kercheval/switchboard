import type { AgentId, AgentRecord } from "./types";

/// Frontend agent-name validation, a deliberate mirror of the backend rules in
/// `crates/core/src/name.rs` (`validate_name` + `canonicalize_for_uniqueness`).
/// It exists purely for live UX — showing an inline error and disabling the
/// commit action before a round-trip. The backend remains authoritative; this
/// must never disagree with it, so the format rule and the canonicalization
/// rule below are kept byte-for-byte equivalent to the Rust side. Do not
/// "simplify" the canonicalization to a literal string compare: names collide
/// case-insensitively and with hyphens treated as underscores.

const ALLOWED_NAME = /^[A-Za-z0-9_-]+$/;

export type NameValidation =
  | { ok: true }
  | { ok: false; reason: "empty"; message: string }
  | { ok: false; reason: "invalid_chars"; message: string }
  /// `collidesWith` (the verbatim name of the existing agent) is required on
  /// the duplicate variant only, so a `reason === "duplicate"` narrow gives
  /// callers a guaranteed `string` and a missing one is a compile error.
  | { ok: false; reason: "duplicate"; message: string; collidesWith: string };

/// Normalize a raw input into the value that is both validated and submitted.
/// Currently just a trim, but the single chokepoint is the point: the backend
/// `validate_name` does not trim (it rejects whitespace as invalid), so the
/// frontend and backend agree only if the *submitted* value is the *validated*
/// value. Both `validateAgentName` and every submit site run input through this
/// — never normalize the bound input field mid-keystroke (that fights the
/// cursor); normalize at validate-time and at submit-time.
export function normalizeAgentName(name: string): string {
  return name.trim();
}

/// Canonicalize a name for the uniqueness check only: hyphens → underscores,
/// then lowercase. Mirrors `canonicalize_for_uniqueness` in core. The verbatim
/// name is what gets stored; this form is compared only to detect collisions.
export function canonicalizeForUniqueness(name: string): string {
  return name.replaceAll("-", "_").toLowerCase();
}

/// Validate a candidate agent name against the format rules and the live
/// roster. The candidate is trimmed first, matching what the create/rename
/// flows submit; the trimmed value is what the backend ultimately validates.
///
/// `excludeAgentId` drops one agent from the uniqueness check — pass the agent
/// being renamed so re-saving its own (unchanged, or case/hyphen-variant) name
/// is not flagged as a self-collision. Omit it when creating.
export function validateAgentName(
  candidate: string,
  roster: AgentRecord[],
  excludeAgentId?: AgentId,
): NameValidation {
  const trimmed = normalizeAgentName(candidate);
  if (trimmed === "") {
    return { ok: false, reason: "empty", message: "Name can't be empty" };
  }
  if (!ALLOWED_NAME.test(trimmed)) {
    return {
      ok: false,
      reason: "invalid_chars",
      message: "Use only letters, numbers, hyphens, and underscores",
    };
  }
  const canonical = canonicalizeForUniqueness(trimmed);
  for (const agent of roster) {
    if (agent.id === excludeAgentId) continue;
    if (canonicalizeForUniqueness(agent.name) === canonical) {
      return {
        ok: false,
        reason: "duplicate",
        message: `An agent named '${agent.name}' already exists`,
        collidesWith: agent.name,
      };
    }
  }
  return { ok: true };
}
