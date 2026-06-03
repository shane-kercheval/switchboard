import { describe, it, expect } from "vitest";
import type { AgentRecord } from "./types";
import { canonicalizeForUniqueness, normalizeAgentName, validateAgentName } from "./agentName";

function agent(id: string, name: string): AgentRecord {
  return {
    id,
    project_id: "project-1",
    name,
    harness: "claude_code",
    session_locator: null,
    created_at: "2026-05-29T00:00:00Z",
  };
}

describe("canonicalizeForUniqueness", () => {
  it("lowercases and normalizes hyphens to underscores", () => {
    expect(canonicalizeForUniqueness("Feature-A")).toBe("feature_a");
    expect(canonicalizeForUniqueness("feature_a")).toBe("feature_a");
    expect(canonicalizeForUniqueness("FEATURE-A")).toBe("feature_a");
    expect(canonicalizeForUniqueness("claude-code")).toBe("claude_code");
  });
});

describe("normalizeAgentName", () => {
  it("trims both ends", () => {
    expect(normalizeAgentName("  assistant  ")).toBe("assistant");
    expect(normalizeAgentName("codex")).toBe("codex");
  });
});

// The format cases below intentionally mirror the backend's own test suite in
// `crates/core/src/name.rs` (`validate_accepts_*` / `validate_rejects_*`) so
// the two sides can't silently drift — a change to one should be a deliberate,
// noticed change to the other.
describe("validateAgentName — format", () => {
  it("rejects empty and whitespace-only names", () => {
    for (const bad of ["", " ", "\t\n", "   "]) {
      const result = validateAgentName(bad, []);
      expect(result).toMatchObject({ ok: false, reason: "empty" });
    }
  });

  it("accepts well-formed names", () => {
    for (const ok of ["assistant", "agent-1", "agent_1", "A", "a", "0", "_", "-", "MixedCase"]) {
      expect(validateAgentName(ok, []), `${ok} should be valid`).toEqual({ ok: true });
    }
  });

  it("accepts leading digit, hyphen, and underscore (no leading-char constraint)", () => {
    expect(validateAgentName("1agent", [])).toEqual({ ok: true });
    expect(validateAgentName("-agent", [])).toEqual({ ok: true });
    expect(validateAgentName("_agent", [])).toEqual({ ok: true });
  });

  it("rejects reserved characters, spaces, and non-ASCII", () => {
    for (const bad of ["agent.1", "agent 1", "agent/1", "agent:1", "agent!", "café", "🤖"]) {
      const result = validateAgentName(bad, []);
      expect(result, `${bad} should be invalid`).toMatchObject({
        ok: false,
        reason: "invalid_chars",
      });
    }
  });

  it("trims before validating, matching what the create/rename flows submit", () => {
    expect(validateAgentName("  assistant  ", [])).toEqual({ ok: true });
  });
});

describe("validateAgentName — uniqueness", () => {
  const roster = [agent("a1", "claude-code"), agent("a2", "Codex")];

  it("flags a duplicate across the canonicalization boundary", () => {
    for (const dup of ["claude-code", "claude_code", "CLAUDE-CODE", "Claude_Code"]) {
      const result = validateAgentName(dup, roster);
      expect(result, `${dup} should collide`).toMatchObject({
        ok: false,
        reason: "duplicate",
        collidesWith: "claude-code",
      });
    }
  });

  it("returns the existing verbatim name in collidesWith", () => {
    const result = validateAgentName("codex", roster);
    expect(result).toMatchObject({ ok: false, reason: "duplicate", collidesWith: "Codex" });
  });

  it("allows a non-colliding name", () => {
    expect(validateAgentName("gemini", roster)).toEqual({ ok: true });
  });
});

describe("validateAgentName — excludeAgentId (rename self-collision)", () => {
  const roster = [agent("a1", "claude-code"), agent("a2", "codex")];

  it("allows re-saving the agent's own name", () => {
    expect(validateAgentName("claude-code", roster, "a1")).toEqual({ ok: true });
  });

  it("allows a case/hyphen variant of the agent's own name", () => {
    expect(validateAgentName("Claude_Code", roster, "a1")).toEqual({ ok: true });
  });

  it("still flags collision with a different agent", () => {
    const result = validateAgentName("codex", roster, "a1");
    expect(result).toMatchObject({ ok: false, reason: "duplicate", collidesWith: "codex" });
  });
});
