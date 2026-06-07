import { describe, expect, it } from "vitest";
import type { HarnessAvailability } from "./types";
import { harnessUnavailableReason, isHarnessSelectable } from "./harnessAvailability";

const CLAUDE_AVAILABLE: HarnessAvailability = { harness: "claude_code", binary: "available" };
const CLAUDE_CHECKING: HarnessAvailability = { harness: "claude_code", binary: "checking" };
const CLAUDE_BINARY_MISSING: HarnessAvailability = { harness: "claude_code", binary: "missing" };
const CODEX_AVAILABLE: HarnessAvailability = { harness: "codex", binary: "available" };
const CODEX_BINARY_MISSING: HarnessAvailability = { harness: "codex", binary: "missing" };
const GEMINI_AVAILABLE: HarnessAvailability = { harness: "gemini", binary: "available" };
const GEMINI_BINARY_MISSING: HarnessAvailability = { harness: "gemini", binary: "missing" };
const ANTIGRAVITY_AVAILABLE: HarnessAvailability = { harness: "antigravity", binary: "available" };
const ANTIGRAVITY_BINARY_MISSING: HarnessAvailability = {
  harness: "antigravity",
  binary: "missing",
};

describe("harnessUnavailableReason", () => {
  it("Claude binary missing returns Claude install copy", () => {
    expect(harnessUnavailableReason(CLAUDE_BINARY_MISSING)).toBe(
      "Claude Code not found on PATH. Install from https://code.claude.com/docs/en/quickstart",
    );
  });

  it("Codex binary missing returns Codex install copy", () => {
    expect(harnessUnavailableReason(CODEX_BINARY_MISSING)).toBe(
      "Codex not found on PATH. Install from https://developers.openai.com/codex/cli",
    );
  });

  it("Gemini binary missing returns Gemini install copy", () => {
    expect(harnessUnavailableReason(GEMINI_BINARY_MISSING)).toBe(
      "Gemini CLI not found on PATH. Install from https://geminicli.com/docs/get-started/installation/",
    );
  });

  it("Antigravity binary missing returns install copy", () => {
    expect(harnessUnavailableReason(ANTIGRAVITY_BINARY_MISSING)).toBe(
      "Antigravity CLI (agy) not found on PATH. Install from https://antigravity.google/docs/cli-install",
    );
  });

  it("available state returns null", () => {
    expect(harnessUnavailableReason(CLAUDE_AVAILABLE)).toBeNull();
    expect(harnessUnavailableReason(CODEX_AVAILABLE)).toBeNull();
    expect(harnessUnavailableReason(GEMINI_AVAILABLE)).toBeNull();
    expect(harnessUnavailableReason(ANTIGRAVITY_AVAILABLE)).toBeNull();
  });

  it("checking state returns null (no scary inline copy during probe window)", () => {
    expect(harnessUnavailableReason(CLAUDE_CHECKING)).toBeNull();
  });
});

describe("isHarnessSelectable", () => {
  it("returns true for binary-available harness", () => {
    expect(isHarnessSelectable(CLAUDE_AVAILABLE)).toBe(true);
    expect(isHarnessSelectable(CODEX_AVAILABLE)).toBe(true);
    expect(isHarnessSelectable(GEMINI_AVAILABLE)).toBe(true);
    expect(isHarnessSelectable(ANTIGRAVITY_AVAILABLE)).toBe(true);
  });

  it("returns false for checking state (closes the pre-probe fail-open window)", () => {
    expect(isHarnessSelectable(CLAUDE_CHECKING)).toBe(false);
  });

  it("returns false for binary missing", () => {
    expect(isHarnessSelectable(CLAUDE_BINARY_MISSING)).toBe(false);
    expect(isHarnessSelectable(CODEX_BINARY_MISSING)).toBe(false);
    expect(isHarnessSelectable(GEMINI_BINARY_MISSING)).toBe(false);
    expect(isHarnessSelectable(ANTIGRAVITY_BINARY_MISSING)).toBe(false);
  });
});
