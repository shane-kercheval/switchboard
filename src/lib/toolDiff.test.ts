import { describe, expect, it } from "vitest";
import type { DiffLine, EditedFile, FileDiff } from "$lib/types";
import { synthesizeEditDiff, synthesizeWriteDiff, truncateDiff } from "$lib/toolDiff";

function file(edits: { old: string; new: string }[], truncated = false): EditedFile {
  return { path: "/repo/src/a.ts", change: "modified", edits, truncated };
}

function line(content: string): DiffLine {
  return { origin: "added", old_lineno: null, new_lineno: 1, content };
}

function fileDiff(hunkSizes: number[]): FileDiff {
  return {
    path: "/repo/src/a.ts",
    binary: false,
    truncated: false,
    too_large: false,
    too_large_bytes: null,
    hunks: hunkSizes.map((n, h) => ({
      header: `@@ hunk ${h} @@`,
      lines: Array.from({ length: n }, (_, i) => line(`h${h}l${i}`)),
    })),
  };
}

describe("synthesizeEditDiff", () => {
  it("renders a one-line change as one removed and one added line", () => {
    const diff = synthesizeEditDiff(file([{ old: "a\nb\nc\n", new: "a\nB\nc\n" }]));

    expect(diff.hunks).toHaveLength(1);
    const origins = diff.hunks[0]!.lines.map((l) => l.origin);
    expect(origins).toEqual(["context", "removed", "added", "context"]);
    expect(diff.hunks[0]!.lines[1]!.content).toBe("b");
    expect(diff.hunks[0]!.lines[2]!.content).toBe("B");
  });

  it("labels every hunk header as snippet-relative", () => {
    const diff = synthesizeEditDiff(file([{ old: "a\n", new: "b\n" }]));
    for (const hunk of diff.hunks) {
      expect(hunk.header).toContain("snippet-relative");
    }
  });

  it("numbers lines relative to the snippet, starting at 1", () => {
    const diff = synthesizeEditDiff(file([{ old: "x\ny\n", new: "x\nz\n" }]));
    const lines = diff.hunks[0]!.lines;
    expect(lines[0]!).toMatchObject({ origin: "context", old_lineno: 1, new_lineno: 1 });
    expect(lines[1]!).toMatchObject({ origin: "removed", old_lineno: 2, new_lineno: null });
    expect(lines[2]!).toMatchObject({ origin: "added", old_lineno: null, new_lineno: 2 });
  });

  it("handles a change with no trailing newline without emitting marker lines", () => {
    const diff = synthesizeEditDiff(file([{ old: "a\nb", new: "a\nc" }]));

    const contents = diff.hunks.flatMap((h) => h.lines.map((l) => l.content));
    expect(contents).toContain("b");
    expect(contents).toContain("c");
    expect(contents.some((c) => c.includes("No newline"))).toBe(false);
  });

  it("renders an addition (empty old) as all added lines", () => {
    const diff = synthesizeEditDiff(file([{ old: "", new: "one\ntwo\n" }]));

    const lines = diff.hunks.flatMap((h) => h.lines).filter((l) => l.content !== "");
    expect(lines.every((l) => l.origin === "added")).toBe(true);
    expect(lines.map((l) => l.content)).toEqual(["one", "two"]);
  });

  it("renders a deletion (empty new) as all removed lines", () => {
    const diff = synthesizeEditDiff(file([{ old: "one\ntwo\n", new: "" }]));

    const lines = diff.hunks.flatMap((h) => h.lines).filter((l) => l.content !== "");
    expect(lines.every((l) => l.origin === "removed")).toBe(true);
    expect(lines.map((l) => l.content)).toEqual(["one", "two"]);
  });

  it("emits one hunk set per edit pair, each restarting snippet numbering", () => {
    const diff = synthesizeEditDiff(
      file([
        { old: "a\n", new: "b\n" },
        { old: "x\n", new: "y\n" },
      ]),
    );

    expect(diff.hunks).toHaveLength(2);
    expect(diff.hunks[0]!.lines[0]!.old_lineno).toBe(1);
    expect(diff.hunks[1]!.lines[0]!.old_lineno).toBe(1);
  });

  it("carries the facet truncation flag onto the FileDiff", () => {
    expect(synthesizeEditDiff(file([{ old: "a\n", new: "b\n" }], true)).truncated).toBe(true);
    expect(synthesizeEditDiff(file([{ old: "a\n", new: "b\n" }], false)).truncated).toBe(false);
  });

  it("produces no hunks for identical before/after content", () => {
    const diff = synthesizeEditDiff(file([{ old: "same\n", new: "same\n" }]));
    expect(diff.hunks).toHaveLength(0);
  });
});

describe("truncateDiff", () => {
  it("returns the diff unchanged when it already fits", () => {
    const diff = fileDiff([3, 2]);
    const result = truncateDiff(diff, 5);
    expect(result.hiddenLines).toBe(0);
    expect(result.diff).toBe(diff); // same reference — no copy
  });

  it("keeps whole hunks up to the cap and drops the rest", () => {
    const result = truncateDiff(fileDiff([3, 3, 3]), 6);
    expect(result.hiddenLines).toBe(3);
    expect(result.diff.hunks).toHaveLength(2);
    expect(result.diff.hunks.flatMap((h) => h.lines)).toHaveLength(6);
  });

  it("trims a hunk that straddles the cap, keeping its header and leading lines", () => {
    const result = truncateDiff(fileDiff([2, 5]), 4);
    expect(result.hiddenLines).toBe(3); // 7 total − 4 kept
    expect(result.diff.hunks).toHaveLength(2);
    expect(result.diff.hunks[1]!.lines).toHaveLength(2);
    expect(result.diff.hunks[1]!.header).toBe("@@ hunk 1 @@");
    // The original diff is not mutated.
    expect(result.diff).not.toBe(fileDiff([2, 5]));
  });
});

describe("synthesizeWriteDiff", () => {
  it("infers creation and emits only added lines without a trailing phantom line", () => {
    const { diff, hiddenLines } = synthesizeWriteDiff("/repo/new.ts", "one\ntwo\n", false);

    expect(hiddenLines).toBe(0);
    expect(diff.hunks[0]!.lines).toEqual([
      { origin: "added", old_lineno: null, new_lineno: 1, content: "one" },
      { origin: "added", old_lineno: null, new_lineno: 2, content: "two" },
    ]);
  });

  it("constructs only the requested preview prefix and reports hidden lines", () => {
    const content = Array.from({ length: 60 }, (_, index) => `line ${index}`).join("\n");
    const { diff, hiddenLines } = synthesizeWriteDiff("/repo/new.ts", content, true, 25);

    expect(diff.hunks[0]!.lines).toHaveLength(25);
    expect(diff.hunks[0]!.lines[24]!.content).toBe("line 24");
    expect(diff.truncated).toBe(true);
    expect(hiddenLines).toBe(35);
  });
});
