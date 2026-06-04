import { describe, expect, it } from "vitest";
import {
  buildRenderArgs,
  combinePromptMessage,
  missingRequiredArgs,
  promptDisplayName,
} from "./prompt";
import type { Prompt } from "./types";

function prompt(args: Prompt["arguments"]): Prompt {
  return {
    provider: "local",
    name: "p",
    title: null,
    description: null,
    arguments: args,
    tags: [],
  };
}

describe("promptDisplayName", () => {
  it("prefers the friendly title", () => {
    expect(promptDisplayName({ title: "Code Review", name: "code-review" })).toBe("Code Review");
  });

  it("falls back to the slug when there is no title", () => {
    expect(promptDisplayName({ title: null, name: "code-review" })).toBe("code-review");
  });
});

describe("combinePromptMessage", () => {
  it("joins the rendered prompt and appended text with a blank line", () => {
    expect(combinePromptMessage("RENDERED", "extra")).toBe("RENDERED\n\nextra");
  });

  it("returns the rendered prompt alone when appended text is empty", () => {
    expect(combinePromptMessage("RENDERED", "")).toBe("RENDERED");
  });

  it("treats whitespace-only appended text as empty (no trailing blank line)", () => {
    expect(combinePromptMessage("RENDERED", "   \n  ")).toBe("RENDERED");
  });

  it("trims the appended text before joining", () => {
    expect(combinePromptMessage("RENDERED", "  hi  ")).toBe("RENDERED\n\nhi");
  });
});

describe("missingRequiredArgs", () => {
  it("flags a required argument left blank", () => {
    const p = prompt([{ name: "focus", description: null, required: true }]);
    expect(missingRequiredArgs(p, {})).toEqual(["focus"]);
    expect(missingRequiredArgs(p, { focus: "" })).toEqual(["focus"]);
    expect(missingRequiredArgs(p, { focus: "   " })).toEqual(["focus"]);
  });

  it("does not flag a filled required argument", () => {
    const p = prompt([{ name: "focus", description: null, required: true }]);
    expect(missingRequiredArgs(p, { focus: "tests" })).toEqual([]);
  });

  it("never flags an optional argument, even when empty", () => {
    const p = prompt([{ name: "tone", description: null, required: false }]);
    expect(missingRequiredArgs(p, {})).toEqual([]);
  });
});

describe("buildRenderArgs", () => {
  const p = prompt([
    { name: "focus", description: null, required: true },
    { name: "tone", description: null, required: false },
  ]);

  it("omits blank optional arguments instead of sending empty strings", () => {
    expect(buildRenderArgs(p, { focus: "tests", tone: "" })).toEqual({ focus: "tests" });
    expect(buildRenderArgs(p, { focus: "tests", tone: "   " })).toEqual({ focus: "tests" });
  });

  it("includes non-blank values verbatim (internal whitespace preserved)", () => {
    expect(buildRenderArgs(p, { focus: "  a  b ", tone: "warm" })).toEqual({
      focus: "  a  b ",
      tone: "warm",
    });
  });

  it("ignores values for arguments the prompt does not declare", () => {
    expect(buildRenderArgs(p, { focus: "x", bogus: "y" })).toEqual({ focus: "x" });
  });
});
