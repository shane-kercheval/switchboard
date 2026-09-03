import { describe, expect, it } from "vitest";
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
  defaultAgentName,
  defaultAgentNameForSelection,
  effortIsRequired,
  effortOptionsFor,
  effortSupportFor,
  resolveModelChange,
  selectionForNewAgent,
  selectionIsValid,
} from "./agentSelection";
import { canonicalizeForUniqueness, validateAgentName } from "./agentName";
import type { AgentSelection } from "./types";

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
      model_choices: ["opus", "sonnet"],
      effort_choices: ["high", "medium"],
      default_model: "opus",
      default_effort: "high",
    });
    expect(DEFAULT_AGENT_SELECTIONS.codex.default_model).toBe("gpt-5.6-sol");
  });
});

describe("Antigravity effort support", () => {
  const values = (model: string | undefined) =>
    effortOptionsFor("antigravity", model).map(({ value }) => value);

  it("distinguishes known levels, explicit no-effort models, and unknown models", () => {
    expect(values("gemini-3.7-flash")).toEqual(["low", "medium", "high"]);
    expect(values("gemini-3.1-pro")).toEqual(["low", "high"]);
    expect(values("gpt-oss-120b")).toEqual(["medium"]);
    expect(effortSupportFor("antigravity", "claude-sonnet-4-6")).toEqual({ kind: "none" });
    expect(effortSupportFor("antigravity", "future-model").kind).toBe("unknown");
  });

  it("classifies every curated Antigravity model explicitly", () => {
    for (const { value } of MODEL_OPTIONS.antigravity) {
      expect(effortSupportFor("antigravity", value).kind, value).not.toBe("unknown");
    }
  });

  it("requires effort only for known effort-bearing Antigravity models", () => {
    expect(effortIsRequired("antigravity", "gemini-3.1-pro")).toBe(true);
    expect(effortIsRequired("antigravity", "claude-sonnet-4-6")).toBe(false);
    expect(effortIsRequired("antigravity", "future-model")).toBe(false);
    expect(effortIsRequired("codex", "gpt-5.6-sol")).toBe(false);
  });

  it("keeps the configured presentation set while restricting activation", () => {
    const configured = selection({
      model: "gemini-3.1-pro",
      effort: "high",
      model_choices: ["gemini-3.1-pro"],
      effort_choices: ["medium", "high"],
    });
    expect(configured.effort_choices).toEqual(["medium", "high"]);
    expect([...activatableEffortValues(configured, "antigravity")]).toEqual(["high"]);
  });
});

describe("selectionForNewAgent", () => {
  it("preserves effort choices but clears the active effort for a no-effort model", () => {
    expect(
      selectionForNewAgent(
        {
          model_choices: ["claude-sonnet-4-6", "gemini-3.7-flash"],
          effort_choices: ["high", "medium"],
          default_model: "claude-sonnet-4-6",
          default_effort: "high",
        },
        "antigravity",
      ),
    ).toEqual({
      ok: true,
      selection: {
        model: "claude-sonnet-4-6",
        effort: null,
        model_choices: ["claude-sonnet-4-6", "gemini-3.7-flash"],
        effort_choices: ["high", "medium"],
      },
    });
  });

  it("resolves an incompatible effort and reports a configuration with no valid choice", () => {
    expect(
      selectionForNewAgent(
        {
          model_choices: ["gemini-3.1-pro"],
          effort_choices: ["medium", "high"],
          default_model: "gemini-3.1-pro",
          default_effort: "medium",
        },
        "antigravity",
      ),
    ).toMatchObject({ ok: true, selection: { effort: "high" } });

    expect(
      selectionForNewAgent(
        {
          model_choices: ["gemini-3.1-pro"],
          effort_choices: ["medium"],
          default_model: "gemini-3.1-pro",
          default_effort: "medium",
        },
        "antigravity",
      ),
    ).toMatchObject({
      ok: false,
      selection: { model: "gemini-3.1-pro", effort: "medium" },
    });
  });
});

describe("resolveModelChange", () => {
  const configured = selection({
    model: "gemini-3.7-flash",
    effort: "medium",
    model_choices: ["gemini-3.7-flash", "gemini-3.1-pro"],
    effort_choices: ["medium", "high", "low"],
  });

  it("preserves valid effort and otherwise chooses the first compatible configured effort", () => {
    expect(
      resolveModelChange({ ...configured, effort: "high" }, "antigravity", "gemini-3.1-pro"),
    ).toMatchObject({ ok: true, selection: { model: "gemini-3.1-pro", effort: "high" } });
    expect(resolveModelChange(configured, "antigravity", "gemini-3.1-pro")).toMatchObject({
      ok: true,
      selection: { model: "gemini-3.1-pro", effort: "low" },
    });
  });

  it("clears effort only for an explicitly known no-effort model", () => {
    expect(resolveModelChange(configured, "antigravity", "claude-sonnet-4-6")).toMatchObject({
      ok: true,
      selection: { model: "claude-sonnet-4-6", effort: null },
    });
    expect(resolveModelChange(configured, "antigravity", "future-model")).toMatchObject({
      ok: true,
      selection: { model: "future-model", effort: "medium" },
    });
  });

  it("refuses a known model when no configured effort can satisfy it", () => {
    expect(
      resolveModelChange(
        { ...configured, effort: "medium", effort_choices: ["medium"] },
        "antigravity",
        "gemini-3.1-pro",
      ),
    ).toEqual({
      ok: false,
      reason: "Add a compatible reasoning effort in Model settings before switching.",
    });
  });

  it("validates known pairs without treating unknown models as invalid", () => {
    expect(selectionIsValid(configured, "antigravity")).toBe(true);
    expect(selectionIsValid({ ...configured, model: "gemini-3.1-pro" }, "antigravity")).toBe(false);
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
