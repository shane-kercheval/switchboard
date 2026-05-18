import { describe, expect, it } from "vitest";
import type { HarnessAvailability, HarnessBanner } from "./types";
import {
  bannerCopy,
  bannerTestid,
  harnessUnavailableReason,
  isHarnessSelectable,
} from "./harnessAvailability";

const CLAUDE_AVAILABLE: HarnessAvailability = {
  harness: "claude_code",
  binary: "available",
  auth: "unsupported",
};
const CLAUDE_CHECKING: HarnessAvailability = {
  harness: "claude_code",
  binary: "checking",
  auth: "unsupported",
};
const CLAUDE_BINARY_MISSING: HarnessAvailability = {
  harness: "claude_code",
  binary: "missing",
  auth: "unsupported",
};
const CODEX_AVAILABLE: HarnessAvailability = {
  harness: "codex",
  binary: "available",
  auth: "available",
};
const CODEX_BINARY_MISSING: HarnessAvailability = {
  harness: "codex",
  binary: "missing",
  auth: "missing",
};
const CODEX_AUTH_MISSING: HarnessAvailability = {
  harness: "codex",
  binary: "available",
  auth: "missing",
};
const CODEX_AUTH_CHECKING: HarnessAvailability = {
  harness: "codex",
  binary: "available",
  auth: "checking",
};
const GEMINI_AVAILABLE: HarnessAvailability = {
  harness: "gemini",
  binary: "available",
  auth: "available",
};
const GEMINI_BINARY_MISSING: HarnessAvailability = {
  harness: "gemini",
  binary: "missing",
  auth: "missing",
};
const GEMINI_AUTH_MISSING: HarnessAvailability = {
  harness: "gemini",
  binary: "available",
  auth: "missing",
};

// **Verbatim-alignment contract**: `bannerCopy` and
// `harnessUnavailableReason` must return identical strings for the same
// underlying gap (binary-missing per harness; Codex auth-missing). Tests
// assert this equality so a future divergent edit fails loudly here
// instead of silently producing inconsistent UX text across banner and
// tooltip.
describe("bannerCopy", () => {
  it("Claude binary_missing surfaces install link", () => {
    const banner: HarnessBanner = { kind: "binary_missing", harness: "claude_code" };
    expect(bannerCopy(banner)).toBe(
      "Claude Code not found on PATH. Install from https://claude.com/code",
    );
  });

  it("Codex binary_missing surfaces install link", () => {
    const banner: HarnessBanner = { kind: "binary_missing", harness: "codex" };
    expect(bannerCopy(banner)).toBe(
      "Codex not found on PATH. Install from https://github.com/openai/codex",
    );
  });

  it("Codex auth_missing surfaces `codex login` guidance", () => {
    const banner: HarnessBanner = { kind: "auth_missing", harness: "codex" };
    expect(bannerCopy(banner)).toBe(
      "Codex not authenticated — run `codex login` and reload Switchboard. (API-key-only auth is not supported.)",
    );
  });

  it("Gemini binary_missing surfaces install link", () => {
    const banner: HarnessBanner = { kind: "binary_missing", harness: "gemini" };
    expect(bannerCopy(banner)).toBe(
      "Gemini CLI not found on PATH. Install from https://github.com/google-gemini/gemini-cli",
    );
  });

  it("Gemini auth_missing surfaces interactive sign-in guidance", () => {
    const banner: HarnessBanner = { kind: "auth_missing", harness: "gemini" };
    expect(bannerCopy(banner)).toBe(
      "Gemini not authenticated — run `gemini` interactively to sign in, then reload Switchboard.",
    );
  });
});

describe("harnessUnavailableReason", () => {
  it("Claude binary missing returns Claude install copy (matches banner verbatim)", () => {
    const reason = harnessUnavailableReason(CLAUDE_BINARY_MISSING);
    expect(reason).toBe(bannerCopy({ kind: "binary_missing", harness: "claude_code" }));
  });

  it("Codex binary missing returns Codex install copy (matches banner verbatim)", () => {
    const reason = harnessUnavailableReason(CODEX_BINARY_MISSING);
    expect(reason).toBe(bannerCopy({ kind: "binary_missing", harness: "codex" }));
  });

  it("Codex auth missing returns auth copy (matches banner verbatim)", () => {
    const reason = harnessUnavailableReason(CODEX_AUTH_MISSING);
    expect(reason).toBe(bannerCopy({ kind: "auth_missing", harness: "codex" }));
  });

  it("Gemini binary missing returns Gemini install copy (matches banner verbatim)", () => {
    const reason = harnessUnavailableReason(GEMINI_BINARY_MISSING);
    expect(reason).toBe(bannerCopy({ kind: "binary_missing", harness: "gemini" }));
  });

  it("Gemini auth missing returns Gemini sign-in copy (matches banner verbatim)", () => {
    const reason = harnessUnavailableReason(GEMINI_AUTH_MISSING);
    expect(reason).toBe(bannerCopy({ kind: "auth_missing", harness: "gemini" }));
  });

  it("available state returns null", () => {
    expect(harnessUnavailableReason(CLAUDE_AVAILABLE)).toBeNull();
    expect(harnessUnavailableReason(CODEX_AVAILABLE)).toBeNull();
    expect(harnessUnavailableReason(GEMINI_AVAILABLE)).toBeNull();
  });

  it("checking state returns null (no scary inline copy during probe window)", () => {
    expect(harnessUnavailableReason(CLAUDE_CHECKING)).toBeNull();
    expect(harnessUnavailableReason(CODEX_AUTH_CHECKING)).toBeNull();
  });
});

describe("isHarnessSelectable", () => {
  it("returns true for fully-available harness", () => {
    expect(isHarnessSelectable(CLAUDE_AVAILABLE)).toBe(true);
    expect(isHarnessSelectable(CODEX_AVAILABLE)).toBe(true);
  });

  it("returns false for checking state (closes the pre-probe fail-open window)", () => {
    expect(isHarnessSelectable(CLAUDE_CHECKING)).toBe(false);
    expect(isHarnessSelectable(CODEX_AUTH_CHECKING)).toBe(false);
  });

  it("returns false for binary missing", () => {
    expect(isHarnessSelectable(CLAUDE_BINARY_MISSING)).toBe(false);
    expect(isHarnessSelectable(CODEX_BINARY_MISSING)).toBe(false);
  });

  it("returns false for auth missing", () => {
    expect(isHarnessSelectable(CODEX_AUTH_MISSING)).toBe(false);
    expect(isHarnessSelectable(GEMINI_AUTH_MISSING)).toBe(false);
  });
});

describe("bannerTestid", () => {
  it("composes kind + harness for stable test selectors", () => {
    expect(bannerTestid({ kind: "binary_missing", harness: "claude_code" })).toBe(
      "banner-binary_missing-claude_code",
    );
    expect(bannerTestid({ kind: "binary_missing", harness: "codex" })).toBe(
      "banner-binary_missing-codex",
    );
    expect(bannerTestid({ kind: "auth_missing", harness: "codex" })).toBe(
      "banner-auth_missing-codex",
    );
  });
});
