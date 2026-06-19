import { describe, expect, it } from "vitest";
import {
  ALL_HARNESSES,
  SUPPORTS_EFFORT_SELECTION,
  SUPPORTS_MODEL_SELECTION,
} from "./harnessDisplay";
import {
  DEFAULT_EFFORT,
  DEFAULT_MODEL,
  EFFORT_OPTIONS,
  MODEL_OPTIONS,
  defaultAgentName,
} from "./agentSelection";
import { canonicalizeForUniqueness, validateAgentName } from "./agentName";

/// The capability fact ("does harness H support axis A") is encoded three times
/// — the capability map, the option list (empty ⇒ unsupported), and the default
/// (undefined ⇒ unsupported) — across two files, intentionally mirroring
/// different sources. Nothing else enforces that the three agree, and the lists
/// are designed to be hand-edited as models ship/sunset. These invariants fail
/// closed on a desync instead of shipping a broken picker: a capability/list
/// mismatch renders a picker with no options, and a default outside its list
/// binds an orphan value the `<select>` never visibly selects.
describe("agentSelection capability tables stay consistent", () => {
  for (const harness of ALL_HARNESSES) {
    it(`${harness}: model capability map, list, and default agree`, () => {
      const supported = SUPPORTS_MODEL_SELECTION[harness];
      expect(MODEL_OPTIONS[harness].length > 0).toBe(supported);
      expect(DEFAULT_MODEL[harness] !== undefined).toBe(supported);
      if (DEFAULT_MODEL[harness] !== undefined) {
        expect(MODEL_OPTIONS[harness].map((option) => option.value)).toContain(
          DEFAULT_MODEL[harness],
        );
      }
    });

    it(`${harness}: effort capability map, list, and default agree`, () => {
      const supported = SUPPORTS_EFFORT_SELECTION[harness];
      expect(EFFORT_OPTIONS[harness].length > 0).toBe(supported);
      expect(DEFAULT_EFFORT[harness] !== undefined).toBe(supported);
      if (DEFAULT_EFFORT[harness] !== undefined) {
        expect(EFFORT_OPTIONS[harness].map((option) => option.value)).toContain(
          DEFAULT_EFFORT[harness],
        );
      }
    });
  }
});

describe("defaultAgentName", () => {
  it("derives model-effort for a fully-capable harness", () => {
    expect(defaultAgentName("claude_code", "opus", "high")).toBe("opus-high");
    expect(defaultAgentName("claude_code", "sonnet", "max")).toBe("sonnet-max");
  });

  it("hyphenates dots in the model id so the name is a valid slug", () => {
    expect(defaultAgentName("codex", "gpt-5.5", "medium")).toBe("gpt-5-5-medium");
    expect(defaultAgentName("codex", "gpt-5.4-mini", "low")).toBe("gpt-5-4-mini-low");
  });

  it("uses just the model when the harness has no effort axis", () => {
    expect(defaultAgentName("gemini", "gemini-2.5-pro", undefined)).toBe("gemini-2-5-pro");
  });

  it("falls back to the bare harness name when the model is auto or absent", () => {
    // Gemini left on `auto` (picks up the last-used model) and Antigravity
    // (model is harness-owned) have no concrete model to name after.
    expect(defaultAgentName("gemini", "auto", undefined)).toBe("gemini");
    expect(defaultAgentName("antigravity", undefined, undefined)).toBe("antigravity");
    // The "keep current" sentinel (attach mode) reads as no model.
    expect(defaultAgentName("claude_code", "", "")).toBe("claude-code");
  });

  // The helper feeds vendor-shaped model ids into a persisted, validated name.
  // Guard the whole curated surface — not just today's defaults — so a future
  // model/effort option carrying a name-illegal character is caught here rather
  // than as an invalid create form / failed auto-seed in production.
  for (const harness of ALL_HARNESSES) {
    const models = MODEL_OPTIONS[harness].length > 0 ? MODEL_OPTIONS[harness] : [{ value: "" }];
    const efforts = EFFORT_OPTIONS[harness].length > 0 ? EFFORT_OPTIONS[harness] : [{ value: "" }];
    for (const model of models) {
      for (const effort of efforts) {
        it(`${harness}: defaultAgentName(${model.value || "∅"}, ${effort.value || "∅"}) is a valid agent name`, () => {
          const name = defaultAgentName(harness, model.value, effort.value);
          expect(validateAgentName(name, [])).toEqual({ ok: true });
        });
      }
    }
  }

  // The previous naming scheme (one slug per harness) guaranteed seeded agents
  // never self-collided by construction; model+effort names don't. New-project
  // auto-seeding creates one agent per installed harness from these static
  // defaults, so a clash would fail one harness's creation. The only point a
  // clash can be introduced is a code edit to the default tables — guard it
  // here under the same canonicalization the backend uses for uniqueness.
  it("seed defaults are pairwise-distinct across harnesses", () => {
    const canonical = ALL_HARNESSES.map((harness) =>
      canonicalizeForUniqueness(
        defaultAgentName(harness, DEFAULT_MODEL[harness], DEFAULT_EFFORT[harness]),
      ),
    );
    expect(new Set(canonical).size).toBe(canonical.length);
  });
});
