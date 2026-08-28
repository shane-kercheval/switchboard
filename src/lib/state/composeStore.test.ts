import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ComposeContent, ComposeForwards, ComposeSnapshot } from "./composeStore";
import {
  _testing,
  clearCompose,
  composeContentMatches,
  draftAttachmentPaths,
  emptyForwards,
  flush,
  getCompose,
  setAttachments,
  setContent,
  setForwards,
  setSelection,
} from "./composeStore";
import type { ForwardSource } from "./heldForwards.svelte";
import type { Attachment } from "$lib/types";

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

const ATTACHMENT: Attachment = {
  label: "image-1",
  kind: "image",
  path: "/abs/.switchboard/projects/p/attachments/u__shot.png",
  original_name: "shot.png",
};

const SOURCE_A: ForwardSource = { id: "agent-a", name: "opus-high" };
const SOURCE_B: ForwardSource = { id: "agent-b", name: "sonnet-low" };

describe("composeStore v3: attachments, forwards, workflow mode", () => {
  it("reads a v2 blob as a v3 snapshot with the newer fields absent", () => {
    // v2 → v3 added only optional fields, so no migration function exists. A v2
    // blob must parse through the same path, not fall into `migrateUnversioned`
    // (which would iterate the envelope's own keys and produce garbage).
    localStorage.setItem(
      STORAGE_KEY,
      JSON.stringify({
        version: 2,
        projects: { [P]: { content: { kind: "plain", draft: "from v2" }, selectedIds: ["a"] } },
      }),
    );
    _testing.reloadFromStorage();
    expect(getCompose(P)).toEqual({
      content: { kind: "plain", draft: "from v2" },
      selectedIds: ["a"],
    });
  });

  it("re-stamps a v2 blob as v3 on the next write", () => {
    localStorage.setItem(
      STORAGE_KEY,
      JSON.stringify({ version: 2, projects: { [P]: { content: { kind: "plain", draft: "x" } } } }),
    );
    _testing.reloadFromStorage();
    setContent(P, { kind: "plain", draft: "y" });
    flush();
    const raw: unknown = JSON.parse(localStorage.getItem(STORAGE_KEY) ?? "{}");
    expect((raw as { version: number }).version).toBe(3);
  });

  it("round-trips staged attachments", () => {
    setAttachments(P, [ATTACHMENT]);
    flush();
    _testing.reloadFromStorage();
    expect(getCompose(P).attachments).toEqual([ATTACHMENT]);
  });

  it("omits the attachments key entirely when the chip list is empty", () => {
    setAttachments(P, [ATTACHMENT]);
    setAttachments(P, []);
    flush();
    _testing.reloadFromStorage();
    expect(getCompose(P).attachments).toBeUndefined();
    expect("attachments" in getCompose(P)).toBe(false);
  });

  it("degrades a malformed attachments array to no attachments rather than throwing", () => {
    localStorage.setItem(
      STORAGE_KEY,
      JSON.stringify({
        version: 3,
        projects: {
          [P]: {
            content: { kind: "plain", draft: "d" },
            attachments: [null, 5, { label: "no-path" }, "nope"],
          },
        },
      }),
    );
    expect(() => _testing.reloadFromStorage()).not.toThrow();
    expect(getCompose(P).attachments).toBeUndefined();
    expect(getCompose(P).content).toEqual({ kind: "plain", draft: "d" });
  });

  it("maps an unrecognized attachment kind to 'unknown' rather than dropping the chip", () => {
    // Cross-version tolerance: a newer build may write a kind this one predates.
    localStorage.setItem(
      STORAGE_KEY,
      JSON.stringify({
        version: 3,
        projects: {
          [P]: {
            content: { kind: "plain", draft: "" },
            attachments: [{ ...ATTACHMENT, kind: "hologram" }],
          },
        },
      }),
    );
    _testing.reloadFromStorage();
    expect(getCompose(P).attachments).toEqual([{ ...ATTACHMENT, kind: "unknown" }]);
  });

  it("round-trips all four forward-source families", () => {
    const forwards: ComposeForwards = {
      message: [SOURCE_A],
      promptArgs: { focus: [SOURCE_B] },
      promptAppended: [SOURCE_A, SOURCE_B],
      workflowFields: { context: [SOURCE_B] },
    };
    setForwards(P, forwards);
    flush();
    _testing.reloadFromStorage();
    expect(getCompose(P).forwards).toEqual(forwards);
  });

  it("round-trips a cross-project source's owning project through storage", () => {
    // The failure this pins: dropping `projectId` on parse makes a foreign source
    // indistinguishable from a legacy local one, and the restore path then filters
    // it out against the current project's roster — the chip is silently deleted
    // on every launch. An in-memory remount cannot catch it; only a real reload can.
    const foreign = {
      id: "agent-z",
      name: "oracle",
      projectId: "00000000-0000-7000-8000-0000000000aa",
      projectName: "backend",
    };
    setForwards(P, {
      message: [foreign],
      promptArgs: { focus: [foreign] },
      promptAppended: [],
      workflowFields: {},
    });
    flush();
    _testing.reloadFromStorage();

    expect(getCompose(P).forwards?.message).toEqual([foreign]);
    expect(getCompose(P).forwards?.promptArgs.focus).toEqual([foreign]);
  });

  it("keeps a legacy source with no project intact rather than inventing one", () => {
    // Drafts written before sources carried an owner. The parser must not guess —
    // the restore path stamps the draft's own project, which is the only correct
    // answer and one the parser doesn't have.
    setForwards(P, {
      message: [{ id: "agent-a", name: "alice" }],
      promptArgs: {},
      promptAppended: [],
      workflowFields: {},
    });
    flush();
    _testing.reloadFromStorage();

    expect(getCompose(P).forwards?.message).toEqual([{ id: "agent-a", name: "alice" }]);
  });

  it("omits the forwards key when every family is empty", () => {
    setForwards(P, { message: [SOURCE_A], promptArgs: {}, promptAppended: [], workflowFields: {} });
    setForwards(P, emptyForwards());
    flush();
    _testing.reloadFromStorage();
    expect(getCompose(P).forwards).toBeUndefined();
  });

  it("drops structurally malformed forward sources but keeps the valid ones", () => {
    localStorage.setItem(
      STORAGE_KEY,
      JSON.stringify({
        version: 3,
        projects: {
          [P]: {
            content: { kind: "plain", draft: "" },
            forwards: { message: [SOURCE_A, { id: 7 }, null, { name: "no-id" }] },
          },
        },
      }),
    );
    _testing.reloadFromStorage();
    expect(getCompose(P).forwards?.message).toEqual([SOURCE_A]);
  });

  it("round-trips workflow-mode content including list-valued inputs", () => {
    // `isBuiltin` is part of the identity: a built-in and a copied user workflow
    // can share a name, so restore needs both to re-resolve the listing.
    setContent(P, {
      kind: "workflow",
      name: "review-and-recommend",
      isBuiltin: true,
      inputs: { context: "check error paths", agents: ["a", "b"] },
    });
    flush();
    _testing.reloadFromStorage();
    expect(getCompose(P).content).toEqual({
      kind: "workflow",
      name: "review-and-recommend",
      isBuiltin: true,
      inputs: { context: "check error paths", agents: ["a", "b"] },
    });
  });

  it("degrades workflow content missing its identity to an empty plain draft", () => {
    localStorage.setItem(
      STORAGE_KEY,
      JSON.stringify({
        version: 3,
        projects: { [P]: { content: { kind: "workflow", name: "x" } } }, // no isBuiltin
      }),
    );
    _testing.reloadFromStorage();
    expect(getCompose(P).content).toEqual({ kind: "plain", draft: "" });
  });

  it("drops workflow inputs whose values are neither string nor string[]", () => {
    localStorage.setItem(
      STORAGE_KEY,
      JSON.stringify({
        version: 3,
        projects: {
          [P]: {
            content: {
              kind: "workflow",
              name: "w",
              isBuiltin: false,
              inputs: { good: "ok", list: ["a"], bad: 5, alsoBad: [1, 2] },
            },
          },
        },
      }),
    );
    _testing.reloadFromStorage();
    expect(getCompose(P).content).toEqual({
      kind: "workflow",
      name: "w",
      isBuiltin: false,
      inputs: { good: "ok", list: ["a"] },
    });
  });

  it("draftAttachmentPaths reports the staged paths the GC must spare", () => {
    expect(draftAttachmentPaths(P)).toEqual([]);
    setAttachments(P, [ATTACHMENT]);
    expect(draftAttachmentPaths(P)).toEqual([ATTACHMENT.path]);
  });

  it("attachments and forwards survive independently of content changes", () => {
    setAttachments(P, [ATTACHMENT]);
    setForwards(P, { ...emptyForwards(), message: [SOURCE_A] });
    setContent(P, { kind: "plain", draft: "typed after attaching" });
    flush();
    _testing.reloadFromStorage();
    const snapshot = getCompose(P);
    expect(snapshot.attachments).toEqual([ATTACHMENT]);
    expect(snapshot.forwards?.message).toEqual([SOURCE_A]);
    expect(snapshot.content).toEqual({ kind: "plain", draft: "typed after attaching" });
  });
});

describe("composeContentMatches: whether a send's composer is still on screen", () => {
  // A send that outlives its own ComposeBar finalizes against this. A false
  // "matches" clears a composer the user has since edited; a false "differs"
  // leaves a prompt that was already sent sitting on screen, one keystroke from
  // being sent twice. Both are silent, so the comparator gets its own tests
  // rather than being covered only through component behavior.
  const PROMPT: ComposeSnapshot = {
    content: {
      kind: "prompt",
      provider: "tiddly",
      name: "summary",
      args: { focus: "tests", scope: "diff" },
      appendedText: "tail",
    },
    selectedIds: ["agent-a"],
    attachments: [ATTACHMENT],
    forwards: { ...emptyForwards(), message: [SOURCE_A] },
  };

  function seed(snapshot: ComposeSnapshot): void {
    setContent(P, snapshot.content);
    setAttachments(P, snapshot.attachments ?? []);
    setForwards(P, snapshot.forwards ?? emptyForwards());
  }

  it("matches a composer that has not changed", () => {
    seed(PROMPT);
    expect(composeContentMatches(P, PROMPT)).toBe(true);
  });

  it("ignores the order arguments were filled in", () => {
    // `args` key order is an artifact of which field the user typed into first,
    // which a plain JSON comparison would report as a difference — and every
    // such false difference silently declines to clear a sent prompt.
    seed(PROMPT);
    expect(
      composeContentMatches(P, {
        ...PROMPT,
        content: {
          kind: "prompt",
          provider: "tiddly",
          name: "summary",
          args: { scope: "diff", focus: "tests" },
          appendedText: "tail",
        },
      }),
    ).toBe(true);
  });

  it("treats an absent optional field as its empty value", () => {
    // The store drops `attachments`/`forwards` entirely when empty, so a capture
    // taken from a pristine composer carries `[]` where the store carries
    // nothing. Those are the same composer.
    setContent(P, { kind: "plain", draft: "" });
    expect(
      composeContentMatches(P, {
        content: { kind: "plain", draft: "" },
        attachments: [],
        forwards: emptyForwards(),
      }),
    ).toBe(true);
  });

  it("notices an edited argument, appended text, or a different prompt", () => {
    seed(PROMPT);
    const content = PROMPT.content as Extract<ComposeContent, { kind: "prompt" }>;
    expect(
      composeContentMatches(P, {
        ...PROMPT,
        content: { ...content, args: { focus: "perf", scope: "diff" } },
      }),
    ).toBe(false);
    expect(
      composeContentMatches(P, { ...PROMPT, content: { ...content, appendedText: "other" } }),
    ).toBe(false);
    expect(composeContentMatches(P, { ...PROMPT, content: { ...content, name: "review" } })).toBe(
      false,
    );
    expect(composeContentMatches(P, { ...PROMPT, content: { kind: "plain", draft: "" } })).toBe(
      false,
    );
  });

  it("notices a staged or removed attachment", () => {
    seed(PROMPT);
    expect(composeContentMatches(P, { ...PROMPT, attachments: [] })).toBe(false);
    setAttachments(P, []);
    expect(composeContentMatches(P, PROMPT)).toBe(false);
  });

  it("notices a forward source added to any family", () => {
    seed(PROMPT);
    expect(
      composeContentMatches(P, {
        ...PROMPT,
        forwards: { ...emptyForwards(), message: [SOURCE_A, SOURCE_B] },
      }),
    ).toBe(false);
    expect(
      composeContentMatches(P, {
        ...PROMPT,
        forwards: { ...emptyForwards(), message: [SOURCE_A], promptAppended: [SOURCE_B] },
      }),
    ).toBe(false);
  });

  it("keeps forward-source order significant", () => {
    // Unlike `args`, a forward list's order is the order sources compose in, so
    // two orderings are genuinely different composers.
    setForwards(P, { ...emptyForwards(), message: [SOURCE_A, SOURCE_B] });
    setContent(P, { kind: "plain", draft: "" });
    expect(
      composeContentMatches(P, {
        content: { kind: "plain", draft: "" },
        forwards: { ...emptyForwards(), message: [SOURCE_B, SOURCE_A] },
      }),
    ).toBe(false);
  });

  it("does not consult recipients — those lag in this snapshot", () => {
    // `selectedIds` here is written by a scheduled effect and trails the live
    // selection by a frame, so a caller that cares must ask recipientSelection.
    seed(PROMPT);
    setSelection(P, ["agent-z"]);
    expect(composeContentMatches(P, PROMPT)).toBe(true);
  });

  it("clearCompose empties content and attachments but keeps the recipients", () => {
    seed(PROMPT);
    setSelection(P, ["agent-a"]);
    clearCompose(P);
    const snapshot = getCompose(P);
    expect(snapshot.content).toEqual({ kind: "plain", draft: "" });
    expect(snapshot.attachments ?? []).toEqual([]);
    expect(snapshot.forwards ?? emptyForwards()).toEqual(emptyForwards());
    expect(snapshot.selectedIds).toEqual(["agent-a"]);
  });
});
