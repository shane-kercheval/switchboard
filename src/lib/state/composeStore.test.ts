import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { _testing, flush, getCompose, setContent, setSelection } from "./composeStore";

const P = "00000000-0000-7000-8000-0000000000ff";
const STORAGE_KEY = "switchboard-compose";

afterEach(() => {
  _testing.reset();
});

describe("composeStore", () => {
  it("round-trips a plain draft and selection through localStorage", () => {
    setContent(P, { kind: "plain", draft: "hello" });
    setSelection(P, ["a", "b"]);
    flush(); // a restart always passes a flush point first (pagehide/destroy)
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
    flush();
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
    flush();
    _testing.reloadFromStorage();
    expect(getCompose(P).selectedIds).toEqual(["a", "b"]);
    expect(getCompose(P).content.kind).toBe("prompt");
  });

  it("distinguishes no-saved-selection (undefined) from deselect-all ([])", () => {
    setContent(P, { kind: "plain", draft: "x" });
    expect(getCompose(P).selectedIds).toBeUndefined();
    setSelection(P, []);
    flush();
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

describe("debounced persistence", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("coalesces a burst of setContent calls into one trailing write", () => {
    const setItem = vi.spyOn(Storage.prototype, "setItem");
    try {
      for (let i = 1; i <= 10; i++) {
        setContent(P, { kind: "plain", draft: "d".repeat(i) });
      }
      expect(setItem).not.toHaveBeenCalled(); // nothing lands while typing
      vi.advanceTimersByTime(250);
      expect(setItem).toHaveBeenCalledTimes(1);
      _testing.reloadFromStorage();
      expect(getCompose(P).content).toEqual({ kind: "plain", draft: "d".repeat(10) });
    } finally {
      setItem.mockRestore();
    }
  });

  it("flush() writes immediately and cancels the pending timer", () => {
    const setItem = vi.spyOn(Storage.prototype, "setItem");
    try {
      setContent(P, { kind: "plain", draft: "x" });
      flush();
      expect(setItem).toHaveBeenCalledTimes(1);
      vi.advanceTimersByTime(1000);
      expect(setItem).toHaveBeenCalledTimes(1); // no second write from the timer
    } finally {
      setItem.mockRestore();
    }
  });

  it("flush() with nothing pending writes nothing", () => {
    const setItem = vi.spyOn(Storage.prototype, "setItem");
    try {
      flush();
      expect(setItem).not.toHaveBeenCalled();
    } finally {
      setItem.mockRestore();
    }
  });

  it("a send-clear followed by debounce expiry never resurrects the cleared draft", () => {
    setContent(P, { kind: "plain", draft: "about to send" }); // pending write
    // The send path: clear + write-through (setContent + flush, as
    // ComposeBar's persistContentNow does).
    setContent(P, { kind: "plain", draft: "" });
    flush();
    vi.advanceTimersByTime(1000); // any stale timer would fire in this window
    _testing.reloadFromStorage();
    expect(getCompose(P).content).toEqual({ kind: "plain", draft: "" });
  });

  it("reads are current while a write is still pending (mutations are synchronous)", () => {
    setContent(P, { kind: "plain", draft: "pending" });
    expect(getCompose(P).content).toEqual({ kind: "plain", draft: "pending" });
  });

  it("coalesces a burst of setSelection calls into one trailing write", () => {
    const setItem = vi.spyOn(Storage.prototype, "setItem");
    try {
      setSelection(P, ["a"]);
      setSelection(P, ["b"]);
      setSelection(P, ["a", "b"]);
      expect(setItem).not.toHaveBeenCalled();
      vi.advanceTimersByTime(250);
      expect(setItem).toHaveBeenCalledTimes(1);
      _testing.reloadFromStorage();
      expect(getCompose(P).selectedIds).toEqual(["a", "b"]);
    } finally {
      setItem.mockRestore();
    }
  });

  it("a fast multi-project burst keeps each draft in its own slot", () => {
    // Both mutations land before the single trailing write fires — the write
    // serializes the whole store at fire time, so neither slot can clobber
    // the other.
    const P2 = "00000000-0000-7000-8000-0000000000aa";
    setContent(P, { kind: "plain", draft: "draft one" });
    setContent(P2, { kind: "plain", draft: "draft two" });
    flush();
    _testing.reloadFromStorage();
    expect(getCompose(P).content).toEqual({ kind: "plain", draft: "draft one" });
    expect(getCompose(P2).content).toEqual({ kind: "plain", draft: "draft two" });
  });

  it("quit events flush once: pagehide then beforeunload writes exactly one snapshot", () => {
    // Real teardown may deliver both events; the second flush must be the
    // documented no-op, not a second serialize+write.
    const setItem = vi.spyOn(Storage.prototype, "setItem");
    try {
      setContent(P, { kind: "plain", draft: "typed just before quit" });
      window.dispatchEvent(new Event("pagehide"));
      window.dispatchEvent(new Event("beforeunload"));
      expect(setItem).toHaveBeenCalledTimes(1);
      _testing.reloadFromStorage();
      expect(getCompose(P).content).toEqual({ kind: "plain", draft: "typed just before quit" });
    } finally {
      setItem.mockRestore();
    }
  });
});
