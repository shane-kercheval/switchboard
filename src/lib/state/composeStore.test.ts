import { afterEach, describe, expect, it } from "vitest";
import { _testing, getCompose, setDraft, setSelection } from "./composeStore";

const P = "00000000-0000-7000-8000-0000000000ff";
const STORAGE_KEY = "switchboard-compose";

afterEach(() => {
  _testing.reset();
});

describe("composeStore", () => {
  it("round-trips draft and selection through localStorage", () => {
    setDraft(P, "hello");
    setSelection(P, ["a", "b"]);
    _testing.reloadFromStorage(); // proves the values survive a fresh hydrate
    expect(getCompose(P)).toEqual({ draft: "hello", selectedIds: ["a", "b"] });
  });

  it("distinguishes no-saved-selection (undefined) from deselect-all ([])", () => {
    setDraft(P, "x");
    expect(getCompose(P).selectedIds).toBeUndefined();
    setSelection(P, []);
    _testing.reloadFromStorage();
    expect(getCompose(P).selectedIds).toEqual([]);
  });

  it("returns an empty snapshot for an unknown project", () => {
    expect(getCompose("unknown")).toEqual({ draft: "" });
  });

  it("starts empty when the stored JSON is malformed", () => {
    localStorage.setItem(STORAGE_KEY, "{not json");
    _testing.reloadFromStorage();
    expect(getCompose(P)).toEqual({ draft: "" });
  });

  it("ignores non-object entries and non-string recipient ids", () => {
    localStorage.setItem(
      STORAGE_KEY,
      JSON.stringify({ [P]: { draft: "d", selectedIds: ["a", 5, null, "b"] }, junk: 7 }),
    );
    _testing.reloadFromStorage();
    expect(getCompose(P)).toEqual({ draft: "d", selectedIds: ["a", "b"] });
    expect(getCompose("junk")).toEqual({ draft: "" });
  });
});
