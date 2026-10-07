import { describe, expect, it } from "vitest";
import { languageForPath, highlightDiffLine, toSideBySide, formatFileSize } from "./diff";
import type { DiffLine } from "./types";

const line = (
  origin: DiffLine["origin"],
  content: string,
  over: Partial<DiffLine> = {},
): DiffLine => ({
  origin,
  old_lineno: null,
  new_lineno: null,
  content,
  ...over,
});

describe("languageForPath", () => {
  it("maps known extensions to Prism grammars", () => {
    expect(languageForPath("src/lib/foo.ts")).toBe("typescript");
    expect(languageForPath("a/b/main.rs")).toBe("rust");
    expect(languageForPath("Cargo.toml")).toBe("toml");
    expect(languageForPath("x.tsx")).toBe("tsx");
  });

  it("returns empty for unknown or extension-less paths", () => {
    expect(languageForPath("bin/tool")).toBe(""); // no extension
    expect(languageForPath(".gitignore")).toBe(""); // dotfile, no extension
    expect(languageForPath("data.xyz")).toBe(""); // unmapped extension
  });
});

describe("highlightDiffLine", () => {
  it("neutralizes embedded HTML so agent-authored content can't execute", () => {
    // The single most important property: a file line containing markup must
    // render as an inert, escaped text node — never a live element.
    const out = highlightDiffLine('<img src=x onerror="alert(1)">', "");
    expect(out).not.toMatch(/<img/i); // no live <img> element
    expect(out).toContain("&lt;img"); // present only as escaped text
  });

  it("does not emit a live script tag, even with a grammar applied", () => {
    const out = highlightDiffLine("<script>alert(1)</script>", "typescript");
    expect(out.toLowerCase()).not.toContain("<script>");
    expect(out.toLowerCase()).not.toContain("</script>");
  });

  it("renders an over-long line as escaped text without Prism markup", () => {
    // A pathological line (a minified bundle on one line) must not be tokenized:
    // it renders as escaped text, so highlighting can't block the main thread.
    const longLine = "const x = 1; ".repeat(1000); // > the 5000-char clamp
    const out = highlightDiffLine(longLine, "typescript");
    expect(out).not.toContain('class="token'); // no Prism tokens emitted
    expect(out.length).toBeGreaterThan(0);
  });

  it("still neutralizes HTML on the over-long-line path", () => {
    const longHostile = "<img src=x onerror=alert(1)> ".repeat(500); // > the clamp
    const out = highlightDiffLine(longHostile, "");
    expect(out).not.toMatch(/<img/i); // no live element
    expect(out).toContain("&lt;img"); // escaped text only
  });
});

describe("formatFileSize", () => {
  it("formats byte counts as short decimal sizes", () => {
    expect(formatFileSize(122_180_589)).toBe("122 MB"); // the reported recording file
    expect(formatFileSize(512)).toBe("512 B");
    expect(formatFileSize(9_800_000)).toBe("9.8 MB");
    expect(formatFileSize(1_500_000_000)).toBe("1.5 GB");
  });

  it("carries into the next unit when rounding hits 1000", () => {
    // 999.96 MB rounds to 1000 in MB → should read "1 GB", not "1000 MB".
    expect(formatFileSize(999_960_000)).toBe("1 GB");
  });
});

describe("toSideBySide", () => {
  it("places a context line on both sides", () => {
    const rows = toSideBySide([line("context", "a", { old_lineno: 1, new_lineno: 1 })]);
    expect(rows).toHaveLength(1);
    expect(rows[0]!.left?.content).toBe("a");
    expect(rows[0]!.right?.content).toBe("a");
  });

  it("pairs a removed run with the following added run row-by-row", () => {
    const rows = toSideBySide([
      line("removed", "old1"),
      line("removed", "old2"),
      line("added", "new1"),
      line("added", "new2"),
    ]);
    expect(rows).toHaveLength(2);
    expect(rows[0]!.left?.content).toBe("old1");
    expect(rows[0]!.right?.content).toBe("new1");
    expect(rows[1]!.left?.content).toBe("old2");
    expect(rows[1]!.right?.content).toBe("new2");
  });

  it("pads the shorter side when removed/added counts differ", () => {
    const moreRemoved = toSideBySide([
      line("removed", "old1"),
      line("removed", "old2"),
      line("added", "new1"),
    ]);
    expect(moreRemoved.map((r) => [r.left?.content ?? null, r.right?.content ?? null])).toEqual([
      ["old1", "new1"],
      ["old2", null],
    ]);

    const moreAdded = toSideBySide([
      line("removed", "old1"),
      line("added", "new1"),
      line("added", "new2"),
    ]);
    expect(moreAdded.map((r) => [r.left?.content ?? null, r.right?.content ?? null])).toEqual([
      ["old1", "new1"],
      [null, "new2"],
    ]);
  });

  it("a pure addition has no left side; a pure deletion has no right", () => {
    expect(toSideBySide([line("added", "x")])).toEqual([
      { left: null, right: expect.objectContaining({ content: "x" }) },
    ]);
    expect(toSideBySide([line("removed", "y")])).toEqual([
      { left: expect.objectContaining({ content: "y" }), right: null },
    ]);
  });

  it("flushes a change block before a trailing context line (no misalignment)", () => {
    const rows = toSideBySide([
      line("context", "ctx-before", { old_lineno: 1, new_lineno: 1 }),
      line("removed", "old"),
      line("added", "new"),
      line("context", "ctx-after", { old_lineno: 3, new_lineno: 3 }),
    ]);
    expect(rows.map((r) => [r.left?.content ?? null, r.right?.content ?? null])).toEqual([
      ["ctx-before", "ctx-before"],
      ["old", "new"],
      ["ctx-after", "ctx-after"],
    ]);
  });
});
