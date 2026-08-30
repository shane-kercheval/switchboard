import { describe, expect, it } from "vitest";
import {
  ALL_HARNESSES,
  SUPPORTS_EFFORT_SELECTION,
  SUPPORTS_MODEL_SELECTION,
} from "./harnessDisplay";
import {
  DEFAULT_AGENT_PROFILES,
  EFFORT_OPTIONS,
  MODEL_OPTIONS,
  defaultAgentName,
  defaultAgentNameForProfiles,
  effortIsRequired,
  effortOptionsFor,
} from "./agentSelection";
import { canonicalizeForUniqueness, validateAgentName } from "./agentName";

/// The capability fact ("does harness H support axis A") is encoded three times
/// — the capability map, the option list (empty ⇒ unsupported), and the built-in
/// default (null ⇒ unsupported) — across two files, intentionally mirroring
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
      const model = DEFAULT_AGENT_PROFILES[harness].primary.model;
      expect(model !== null).toBe(supported);
      if (model !== null) {
        expect(MODEL_OPTIONS[harness].map((option) => option.value)).toContain(model);
      }
    });

    it(`${harness}: effort capability map, list, and default agree`, () => {
      const supported = SUPPORTS_EFFORT_SELECTION[harness];
      expect(EFFORT_OPTIONS[harness].length > 0).toBe(supported);
      const effort = DEFAULT_AGENT_PROFILES[harness].primary.effort;
      expect(effort !== null).toBe(supported);
      if (effort !== null) {
        expect(EFFORT_OPTIONS[harness].map((option) => option.value)).toContain(effort);
      }
    });
  }

  it("defaults Antigravity to Pro high with Flash high as secondary", () => {
    expect(DEFAULT_AGENT_PROFILES.antigravity).toEqual({
      primary: { model: "gemini-3.1-pro", effort: "high" },
      secondary: { model: "gemini-3.7-flash", effort: "high" },
    });
  });

  it("enables a secondary profile for every harness", () => {
    for (const harness of ALL_HARNESSES) {
      expect(DEFAULT_AGENT_PROFILES[harness].secondary, harness).not.toBeNull();
    }
  });
});

describe("effortOptionsFor", () => {
  const values = (harness: Parameters<typeof effortOptionsFor>[0], model: string | undefined) =>
    effortOptionsFor(harness, model).map((o) => o.value);

  it("offers every Codex level regardless of model, curated or not", () => {
    // Codex effort validity is server-enforced and self-describing, so the
    // picker stays permissive and lets an invalid level fail the turn.
    const all = EFFORT_OPTIONS.codex.map((o) => o.value);
    for (const model of ["gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna", "gpt-5.5", undefined]) {
      expect(values("codex", model)).toEqual(all);
    }
  });

  it("returns the harness list unchanged for non-model-dependent harnesses", () => {
    expect(effortOptionsFor("claude_code", "opus")).toEqual(EFFORT_OPTIONS.claude_code);
  });

  /// Antigravity's effort axis is per-model in a way no other harness's is: the
  /// levels differ by model, some models have none at all, and where the axis
  /// exists it is **mandatory**. Getting any of these wrong fails the dispatch
  /// before a model runs (`agy` validates client-side), so each shape is
  /// pinned rather than left to the picker to discover.
  it("derives Antigravity effort per model, including the no-axis and single-level shapes", () => {
    expect(values("antigravity", "gemini-3.7-flash")).toEqual(["low", "medium", "high"]);
    // 3.1 Pro genuinely lacks medium — the sets differ, which is why the map is
    // per-model rather than one list.
    expect(values("antigravity", "gemini-3.1-pro")).toEqual(["low", "high"]);

    // The two Claude models have no axis: `agy` rejects `--effort` outright, so
    // an empty list is what hides the control.
    expect(values("antigravity", "claude-sonnet-4-6")).toEqual([]);
    expect(values("antigravity", "claude-opus-4-6-thinking")).toEqual([]);

    // GPT-OSS accepts exactly one level. It must NOT be empty — an empty list
    // hides the control, and hiding it is what used to leave the picker
    // disagreeing with the turn footer, which renders `medium` regardless.
    expect(values("antigravity", "gpt-oss-120b")).toEqual(["medium"]);
  });

  it("makes Antigravity effort mandatory exactly where an axis exists", () => {
    for (const model of ["gemini-3.7-flash", "gemini-3.1-pro", "gpt-oss-120b"]) {
      expect(effortIsRequired("antigravity", model), model).toBe(true);
    }
    for (const model of ["claude-sonnet-4-6", "claude-opus-4-6-thinking", undefined]) {
      expect(effortIsRequired("antigravity", model), String(model)).toBe(false);
    }
    // Every other harness treats effort as optional — unset means "pass no
    // flag." The negative half is what keeps this from becoming a global rule.
    expect(effortIsRequired("claude_code", "opus")).toBe(false);
    expect(effortIsRequired("codex", "gpt-5.6-sol")).toBe(false);
  });

  it("offers only models agy still accepts, and no retired Flash generation", () => {
    const slugs = MODEL_OPTIONS.antigravity.map((o) => o.value);
    expect(slugs).not.toContain("gemini-3.6-flash");
    expect(slugs).not.toContain("gemini-3.5-flash");
    expect(slugs).toContain("gemini-3.7-flash");
    // Effort belongs to the effort control, never folded into a model label —
    // otherwise the label contradicts the footer beneath it.
    for (const { label } of MODEL_OPTIONS.antigravity) {
      expect(label, label).not.toMatch(/\((Low|Medium|High)\)$/);
    }
  });

  it("keeps medium available as the safe clamp target for every curated Codex model", () => {
    for (const { value: model } of MODEL_OPTIONS.codex) {
      expect(values("codex", model)).toContain("medium");
    }
  });
});

describe("built-in agent defaults", () => {
  for (const harness of ALL_HARNESSES) {
    it(`${harness}: uses supported model and effort values`, () => {
      const defaults = DEFAULT_AGENT_PROFILES[harness];
      for (const profile of [defaults.primary, defaults.secondary]) {
        expect(profile).not.toBeNull();
        const { model, effort } = profile!;
        if (model !== null) {
          expect(MODEL_OPTIONS[harness].map((option) => option.value)).toContain(model);
        }
        if (effort !== null) {
          expect(
            effortOptionsFor(harness, model ?? undefined).map((option) => option.value),
          ).toContain(effort);
        }
      }
    });
  }

  it("seeds Codex with Sol and high effort", () => {
    expect(DEFAULT_AGENT_PROFILES.codex.primary).toEqual({
      model: "gpt-5.6-sol",
      effort: "high",
    });
  });
});

describe("defaultAgentName", () => {
  it("derives model-effort for a fully-capable harness", () => {
    expect(defaultAgentName("claude_code", "opus", "high")).toBe("opus-high");
    expect(defaultAgentName("claude_code", "sonnet", "max")).toBe("sonnet-max");
  });

  it("hyphenates dots in the model id so the name is a valid slug", () => {
    expect(defaultAgentName("codex", "gpt-5.6-terra", "medium")).toBe("gpt-5-6-terra-medium");
    expect(defaultAgentName("codex", "gpt-5.5", "medium")).toBe("gpt-5-5-medium");
    expect(defaultAgentName("codex", "gpt-5.6-luna", "low")).toBe("gpt-5-6-luna-low");
  });

  it("uses just the model when the harness has no effort axis", () => {
    expect(defaultAgentName("gemini", "gemini-2.5-pro", undefined)).toBe("gemini-2-5-pro");
  });

  it("uses the short harness name when a secondary profile is configured", () => {
    expect(
      defaultAgentNameForProfiles(
        "claude_code",
        { model: "opus", effort: "high" },
        { model: "sonnet", effort: "medium" },
      ),
    ).toBe("claude");
    expect(
      defaultAgentNameForProfiles(
        "codex",
        { model: "gpt-5.6-sol", effort: "high" },
        { model: "gpt-5.6-terra", effort: "medium" },
      ),
    ).toBe("codex");
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
          expect(
            validateAgentName(
              defaultAgentNameForProfiles(
                harness,
                { model: model.value, effort: effort.value },
                { model: model.value, effort: effort.value },
              ),
              [],
            ),
          ).toEqual({ ok: true });
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
  it("new-project seed defaults are pairwise-distinct across harnesses", () => {
    const canonical = ALL_HARNESSES.map((harness) =>
      canonicalizeForUniqueness(
        defaultAgentName(
          harness,
          DEFAULT_AGENT_PROFILES[harness].primary.model ?? undefined,
          DEFAULT_AGENT_PROFILES[harness].primary.effort ?? undefined,
        ),
      ),
    );
    expect(new Set(canonical).size).toBe(canonical.length);
  });
});
