import { describe, expect, it, vi } from "vitest";
import {
  ALL_HARNESSES,
  SUPPORTS_EFFORT_SELECTION,
  SUPPORTS_MODEL_SELECTION,
} from "./harnessDisplay";
import {
  DEFAULT_AGENT_SELECTIONS,
  EFFORT_OPTIONS,
  MODEL_OPTIONS,
  activatableEffortValues,
  claudeModelFamilyLabel,
  defaultAgentName,
  defaultAgentNameForSelection,
  effortIsRequired,
  effortOptionsFor,
  effortSupportFor,
  resolveModelChange,
  selectionForNewAgent,
  selectionIsValid,
} from "./agentSelection";
import antigravityModels from "./antigravityModels.json";
import { canonicalizeForUniqueness, validateAgentName } from "./agentName";
import type { AgentSelection } from "./types";

// No model agy offers today lacks an effort axis, but the pickers still handle
// one (agy 1.1.19 offered two), so these tests add one to the model table.
vi.mock("./antigravityModels.json", async (importOriginal) => {
  const actual = await importOriginal<{ default: { models: unknown[] } }>();
  return {
    default: {
      ...actual.default,
      models: [
        ...actual.default.models,
        { value: "no-effort-model", label: "No Effort Model", efforts: null },
      ],
    },
  };
});

function selection(over: Partial<AgentSelection> = {}): AgentSelection {
  return {
    model: "opus",
    effort: "high",
    model_choices: ["opus", "sonnet"],
    effort_choices: ["high", "medium"],
    ...over,
  };
}

describe("agent selection catalogs", () => {
  for (const harness of ALL_HARNESSES) {
    it(`${harness}: capabilities, options, and defaults agree`, () => {
      const defaults = DEFAULT_AGENT_SELECTIONS[harness];
      expect(MODEL_OPTIONS[harness].length > 0).toBe(SUPPORTS_MODEL_SELECTION[harness]);
      expect(EFFORT_OPTIONS[harness].length > 0).toBe(SUPPORTS_EFFORT_SELECTION[harness]);
      expect(defaults.model_choices).toContain(defaults.default_model);
      expect(defaults.effort_choices).toContain(defaults.default_effort);
      expect(
        effortOptionsFor(harness, defaults.default_model ?? undefined).map(({ value }) => value),
      ).toContain(defaults.default_effort);
      expect(MODEL_OPTIONS[harness].map(({ value }) => value)).toEqual(
        expect.arrayContaining(defaults.model_choices),
      );
      expect(EFFORT_OPTIONS[harness].map(({ value }) => value)).toEqual(
        expect.arrayContaining(defaults.effort_choices),
      );
    });
  }

  it("ships two independent choices for Claude and Codex", () => {
    expect(DEFAULT_AGENT_SELECTIONS.claude_code).toEqual({
      model_choices: ["fable", "opus"],
      effort_choices: ["medium", "high"],
      default_model: "opus",
      default_effort: "medium",
    });
    expect(DEFAULT_AGENT_SELECTIONS.codex).toEqual({
      model_choices: ["gpt-6.1-sol", "gpt-5.6-terra"],
      effort_choices: ["medium", "high"],
      default_model: "gpt-6.1-sol",
      default_effort: "medium",
    });
    expect(DEFAULT_AGENT_SELECTIONS.antigravity).toEqual({
      model_choices: ["gemini-3.8-flash", "gemini-4-argon"],
      effort_choices: ["medium", "high"],
      default_model: "gemini-3.8-flash",
      default_effort: "medium",
    });
  });

  it("offers only reasoning-bearing Codex effort levels", () => {
    expect(EFFORT_OPTIONS.codex.map(({ value }) => value)).toEqual([
      "low",
      "medium",
      "high",
      "xhigh",
      "max",
      "ultra",
    ]);
  });

  it("offers the GPT-6 family plus GPT-5.6 Terra, which has no GPT-6 counterpart", () => {
    expect(MODEL_OPTIONS.codex).toEqual([
      { label: "GPT-6 Astra", value: "gpt-6-astra" },
      { label: "GPT-6.1 Sol", value: "gpt-6.1-sol" },
      { label: "GPT-5.6 Terra", value: "gpt-5.6-terra" },
      { label: "GPT-6 Luna", value: "gpt-6-luna" },
    ]);
  });
});

describe("Antigravity effort support", () => {
  const values = (model: string | undefined) =>
    effortOptionsFor("antigravity", model).map(({ value }) => value);

  it("distinguishes known levels, explicit no-effort models, and unknown models", () => {
    expect(values("gemini-3.8-flash")).toEqual(["low", "medium", "high"]);
    expect(values("gpt-oss-120b")).toEqual(["medium"]);
    expect(effortSupportFor("antigravity", "no-effort-model")).toEqual({ kind: "none" });
    expect(effortSupportFor("antigravity", "future-model").kind).toBe("unknown");
  });

  it("classifies every curated Antigravity model explicitly", () => {
    for (const { value } of MODEL_OPTIONS.antigravity) {
      expect(effortSupportFor("antigravity", value).kind, value).not.toBe("unknown");
    }
  });

  it("offers every effort level the model table lists", () => {
    const models: readonly { value: string; efforts: readonly string[] | null }[] =
      antigravityModels.models;
    for (const { value, efforts } of models) {
      expect(values(value), value).toHaveLength(efforts?.length ?? 0);
    }
  });

  it("requires effort only for known effort-bearing Antigravity models", () => {
    expect(effortIsRequired("antigravity", "gpt-oss-120b")).toBe(true);
    expect(effortIsRequired("antigravity", "no-effort-model")).toBe(false);
    expect(effortIsRequired("antigravity", "future-model")).toBe(false);
    expect(effortIsRequired("codex", "gpt-5.6-sol")).toBe(false);
  });

  it("keeps the configured presentation set while restricting activation", () => {
    const configured = selection({
      model: "gpt-oss-120b",
      effort: "medium",
      model_choices: ["gpt-oss-120b"],
      effort_choices: ["medium", "high"],
    });
    expect(configured.effort_choices).toEqual(["medium", "high"]);
    expect([...activatableEffortValues(configured, "antigravity")]).toEqual(["medium"]);
  });
});

describe("selectionForNewAgent", () => {
  it("preserves effort choices but clears the active effort for a no-effort model", () => {
    expect(
      selectionForNewAgent(
        {
          model_choices: ["no-effort-model", "gemini-3.8-flash"],
          effort_choices: ["high", "medium"],
          default_model: "no-effort-model",
          default_effort: "high",
        },
        "antigravity",
      ),
    ).toEqual({
      ok: true,
      selection: {
        model: "no-effort-model",
        effort: null,
        model_choices: ["no-effort-model", "gemini-3.8-flash"],
        effort_choices: ["high", "medium"],
      },
    });
  });

  it("resolves an incompatible effort and reports a configuration with no valid choice", () => {
    expect(
      selectionForNewAgent(
        {
          model_choices: ["gpt-oss-120b"],
          effort_choices: ["high", "medium"],
          default_model: "gpt-oss-120b",
          default_effort: "high",
        },
        "antigravity",
      ),
    ).toMatchObject({ ok: true, selection: { effort: "medium" } });

    expect(
      selectionForNewAgent(
        {
          model_choices: ["gpt-oss-120b"],
          effort_choices: ["high"],
          default_model: "gpt-oss-120b",
          default_effort: "high",
        },
        "antigravity",
      ),
    ).toMatchObject({
      ok: false,
      selection: { model: "gpt-oss-120b", effort: "high" },
    });
  });
});

describe("resolveModelChange", () => {
  const configured = selection({
    model: "gemini-3.8-flash",
    effort: "high",
    model_choices: ["gemini-3.8-flash", "gpt-oss-120b"],
    effort_choices: ["high", "medium", "low"],
  });

  it("preserves valid effort and otherwise chooses the first compatible configured effort", () => {
    expect(resolveModelChange(configured, "antigravity", "gemini-4-argon")).toMatchObject({
      ok: true,
      selection: { model: "gemini-4-argon", effort: "high" },
    });
    expect(resolveModelChange(configured, "antigravity", "gpt-oss-120b")).toMatchObject({
      ok: true,
      selection: { model: "gpt-oss-120b", effort: "medium" },
    });
    // "First" is in level order, not quick-choice order: `low` before `high`.
    expect(
      resolveModelChange(
        {
          ...configured,
          model: "future-model",
          effort: "max",
          effort_choices: ["max", "high", "low"],
        },
        "antigravity",
        "gemini-3.8-flash",
      ),
    ).toMatchObject({ ok: true, selection: { model: "gemini-3.8-flash", effort: "low" } });
  });

  it("clears effort only for an explicitly known no-effort model", () => {
    expect(resolveModelChange(configured, "antigravity", "no-effort-model")).toMatchObject({
      ok: true,
      selection: { model: "no-effort-model", effort: null },
    });
    expect(resolveModelChange(configured, "antigravity", "future-model")).toMatchObject({
      ok: true,
      selection: { model: "future-model", effort: "high" },
    });
  });

  it("refuses a known model when no configured effort can satisfy it", () => {
    expect(
      resolveModelChange(
        { ...configured, effort: "high", effort_choices: ["high"] },
        "antigravity",
        "gpt-oss-120b",
      ),
    ).toEqual({
      ok: false,
      reason: "No configured reasoning effort is compatible with that model.",
    });
  });

  it("validates known pairs without treating unknown models as invalid", () => {
    expect(selectionIsValid(configured, "antigravity")).toBe(true);
    expect(selectionIsValid({ ...configured, model: "gpt-oss-120b" }, "antigravity")).toBe(false);
    expect(selectionIsValid({ ...configured, model: "future-model" }, "antigravity")).toBe(true);
  });
});

describe("default agent naming", () => {
  it("uses model and effort only when neither axis has multiple choices", () => {
    expect(
      defaultAgentNameForSelection(
        "claude_code",
        selection({ model_choices: ["opus"], effort_choices: ["high"] }),
      ),
    ).toBe("opus-high");
    expect(defaultAgentNameForSelection("claude_code", selection())).toBe("claude");
    expect(
      defaultAgentNameForSelection(
        "claude_code",
        selection({ model_choices: ["opus"], effort_choices: ["high", "medium"] }),
      ),
    ).toBe("claude");
  });

  it("slugifies vendor model ids and falls back when no model is selected", () => {
    expect(defaultAgentName("codex", "gpt-5.6-terra", "medium")).toBe("gpt-5-6-terra-medium");
    expect(defaultAgentName("antigravity", undefined, undefined)).toBe("antigravity");
  });

  for (const harness of ALL_HARNESSES) {
    it(`${harness}: every built-in name is valid`, () => {
      const defaults = DEFAULT_AGENT_SELECTIONS[harness];
      const name = defaultAgentNameForSelection(harness, {
        model: defaults.default_model,
        effort: defaults.default_effort,
        model_choices: defaults.model_choices,
        effort_choices: defaults.effort_choices,
      });
      expect(validateAgentName(name, [])).toEqual({ ok: true });
    });
  }

  it("new-project seed defaults remain distinct across harnesses", () => {
    const names = ALL_HARNESSES.map((harness) => {
      const defaults = DEFAULT_AGENT_SELECTIONS[harness];
      return canonicalizeForUniqueness(
        defaultAgentNameForSelection(harness, {
          model: defaults.default_model,
          effort: defaults.default_effort,
          model_choices: defaults.model_choices,
          effort_choices: defaults.effort_choices,
        }),
      );
    });
    expect(new Set(names).size).toBe(names.length);
  });
});

describe("claudeModelFamilyLabel", () => {
  it("maps an observed model id to the family name the picker uses", () => {
    // The stream reports full ids; the picker is keyed by family alias, and the
    // card should name the model with the word the user selected it by.
    expect(claudeModelFamilyLabel("claude-fable-5-1")).toBe("Fable");
    expect(claudeModelFamilyLabel("claude-opus-5")).toBe("Opus");
    expect(claudeModelFamilyLabel("claude-sonnet-5")).toBe("Sonnet");
    expect(claudeModelFamilyLabel("claude-haiku-4-5-20251001")).toBe("Haiku");
  });

  it("returns an unrecognized id verbatim", () => {
    // Long, but never a guess: a future family gets its real id until the
    // picker learns the name.
    expect(claudeModelFamilyLabel("claude-zephyr-7")).toBe("claude-zephyr-7");
  });

  it("matches whole segments, not substrings", () => {
    // "opus" inside another token is not the Opus family.
    expect(claudeModelFamilyLabel("claude-opusculum-1")).toBe("claude-opusculum-1");
  });
});
