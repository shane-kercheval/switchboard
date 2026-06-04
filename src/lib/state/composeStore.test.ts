import { afterEach, describe, expect, it } from "vitest";
import { _testing, getCompose, setContent, setSelection } from "./composeStore";

const P = "00000000-0000-7000-8000-0000000000ff";
const STORAGE_KEY = "switchboard-compose";

afterEach(() => {
  _testing.reset();
});

describe("composeStore", () => {
  it("round-trips a plain draft and selection through localStorage", () => {
    setContent(P, { kind: "plain", draft: "hello" });
    setSelection(P, ["a", "b"]);
    _testing.reloadFromStorage(); // proves the values survive a fresh hydrate
    expect(getCompose(P)).toEqual({
      content: { kind: "plain", draft: "hello" },
      selectedIds: ["a", "b"],
    });
  });

  it("round-trips prompt-mode content (provider, name, args, appended text)", () => {
    setContent(P, {
      kind: "prompt",
      provider: "local",
      name: "review",
      args: { focus: "tests" },
      appendedText: "also check error paths",
    });
    setSelection(P, ["a"]);
    _testing.reloadFromStorage();
    expect(getCompose(P)).toEqual({
      content: {
        kind: "prompt",
        provider: "local",
        name: "review",
        args: { focus: "tests" },
        appendedText: "also check error paths",
      },
      selectedIds: ["a"],
    });
  });

  it("keeps recipient selection across a plain↔prompt content switch", () => {
    setSelection(P, ["a", "b"]);
    setContent(P, { kind: "plain", draft: "x" });
    setContent(P, {
      kind: "prompt",
      provider: "local",
      name: "p",
      args: {},
      appendedText: "",
    });
    _testing.reloadFromStorage();
    expect(getCompose(P).selectedIds).toEqual(["a", "b"]);
    expect(getCompose(P).content.kind).toBe("prompt");
  });

  it("distinguishes no-saved-selection (undefined) from deselect-all ([])", () => {
    setContent(P, { kind: "plain", draft: "x" });
    expect(getCompose(P).selectedIds).toBeUndefined();
    setSelection(P, []);
    _testing.reloadFromStorage();
    expect(getCompose(P).selectedIds).toEqual([]);
  });

  it("returns an empty plain snapshot for an unknown project", () => {
    expect(getCompose("unknown")).toEqual({ content: { kind: "plain", draft: "" } });
  });

  it("starts empty when the stored JSON is malformed", () => {
    localStorage.setItem(STORAGE_KEY, "{not json");
    _testing.reloadFromStorage();
    expect(getCompose(P)).toEqual({ content: { kind: "plain", draft: "" } });
  });

  it("migrates a legacy unversioned blob to plain content", () => {
    // The pre-versioning shape: a flat map of `{ draft, selectedIds }`.
    localStorage.setItem(
      STORAGE_KEY,
      JSON.stringify({ [P]: { draft: "legacy", selectedIds: ["a", "b"] } }),
    );
    _testing.reloadFromStorage();
    expect(getCompose(P)).toEqual({
      content: { kind: "plain", draft: "legacy" },
      selectedIds: ["a", "b"],
    });
  });

  it("degrades a malformed prompt content to an empty plain draft", () => {
    localStorage.setItem(
      STORAGE_KEY,
      JSON.stringify({
        version: 2,
        projects: { [P]: { content: { kind: "prompt", provider: "local" } } }, // missing fields
      }),
    );
    _testing.reloadFromStorage();
    expect(getCompose(P)).toEqual({ content: { kind: "plain", draft: "" } });
  });

  it("ignores non-string recipient ids within a versioned blob", () => {
    localStorage.setItem(
      STORAGE_KEY,
      JSON.stringify({
        version: 2,
        projects: {
          [P]: { content: { kind: "plain", draft: "d" }, selectedIds: ["a", 5, null, "b"] },
        },
      }),
    );
    _testing.reloadFromStorage();
    expect(getCompose(P)).toEqual({
      content: { kind: "plain", draft: "d" },
      selectedIds: ["a", "b"],
    });
  });
});
