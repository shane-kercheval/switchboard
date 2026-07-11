import { describe, expect, it } from "vitest";
import type { EditedFile } from "$lib/types";
import { synthesizeEditDiff } from "$lib/toolDiff";

function file(edits: { old: string; new: string }[], truncated = false): EditedFile {
  return { path: "/repo/src/a.ts", change: "modified", edits, truncated };
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
