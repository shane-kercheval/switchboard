import { describe, expect, it, vi } from "vitest";
import type { DiffLine, EditedFile, FileDiff } from "$lib/types";
import {
  COLLAPSED_EDIT_DIFF_TIMEOUT_MS,
  EXPANDED_EDIT_DIFF_TIMEOUT_MS,
  createExpandedDiffCoordinator,
  synthesizeCollapsedEditDiffs,
  synthesizeEditDiff,
  synthesizeEditDiffAsync,
  synthesizeMcpTextCreationDiff,
  synthesizeMcpTextEditDiff,
  synthesizeWriteDiff,
  truncateDiff,
} from "$lib/toolDiff";

const structuredPatchSpy = vi.hoisted(() => vi.fn());
vi.mock("diff", async (importOriginal) => {
  const mod = await importOriginal<typeof import("diff")>();
  structuredPatchSpy.mockImplementation(mod.structuredPatch);
  return { ...mod, structuredPatch: structuredPatchSpy };
});

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

describe("abortable edit synthesis", () => {
  it("returns no collapsed diff when jsdiff reaches the computation deadline", () => {
    structuredPatchSpy.mockReturnValueOnce(undefined);

    const result = synthesizeMcpTextEditDiff(
      "before",
      "after",
      false,
      COLLAPSED_EDIT_DIFF_TIMEOUT_MS,
    );

    expect(result).toBeUndefined();
    expect(structuredPatchSpy).toHaveBeenLastCalledWith(
      "",
      "",
      "before",
      "after",
      undefined,
      undefined,
      expect.objectContaining({ timeout: expect.any(Number) }),
    );
    const timeout = (structuredPatchSpy.mock.calls.at(-1)?.[6] as { timeout: number }).timeout;
    expect(timeout).toBeGreaterThan(0);
    expect(timeout).toBeLessThanOrEqual(COLLAPSED_EDIT_DIFF_TIMEOUT_MS);
  });

  it("keeps a large simple addition eligible for an inline preview", () => {
    const result = synthesizeMcpTextEditDiff(
      "",
      "x".repeat(100_000),
      false,
      COLLAPSED_EDIT_DIFF_TIMEOUT_MS,
    );

    expect(result).toBeDefined();
    expect(result?.hunks.flatMap((hunk) => hunk.lines)).toHaveLength(1);
  });

  it("returns no expanded diff when the asynchronous safety ceiling is reached", async () => {
    structuredPatchSpy.mockImplementationOnce((...args: unknown[]) => {
      const options = args[6] as { timeout: number; callback: (patch: undefined) => void };
      expect(options.timeout).toBeGreaterThan(0);
      expect(options.timeout).toBeLessThanOrEqual(EXPANDED_EDIT_DIFF_TIMEOUT_MS);
      options.callback(undefined);
      return undefined;
    });

    await expect(synthesizeEditDiffAsync(file([{ old: "before", new: "after" }]))).resolves.toBe(
      undefined,
    );
  });

  it("shares one collapsed deadline across files and does not start work after exhaustion", () => {
    structuredPatchSpy.mockClear();
    let now = 0;
    structuredPatchSpy
      .mockImplementationOnce((...args: unknown[]) => {
        const options = args[6] as { timeout: number };
        expect(options.timeout).toBe(25);
        now = 10;
        return {
          hunks: [{ oldStart: 1, oldLines: 1, newStart: 1, newLines: 1, lines: ["-a", "+b"] }],
        };
      })
      .mockImplementationOnce((...args: unknown[]) => {
        const options = args[6] as { timeout: number };
        expect(options.timeout).toBe(15);
        now = 26;
        return {
          hunks: [{ oldStart: 1, oldLines: 1, newStart: 1, newLines: 1, lines: ["-c", "+d"] }],
        };
      });

    const files = ["a", "b", "c"].map((name) => ({
      path: `/repo/${name}.ts`,
      change: "modified" as const,
      edits: [{ old: `${name}-old`, new: `${name}-new` }],
      truncated: false,
    }));
    const results = synthesizeCollapsedEditDiffs(files, 25, () => now);

    expect(results[0]).toBeDefined();
    expect(results[1]).toBeUndefined();
    expect(results[2]).toBeUndefined();
    expect(structuredPatchSpy).toHaveBeenCalledTimes(2);
  });

  it("includes patch conversion in the collapsed deadline", () => {
    structuredPatchSpy.mockClear();
    let clockReads = 0;
    const now = (): number => {
      clockReads += 1;
      return clockReads >= 5 ? 26 : 0;
    };
    structuredPatchSpy.mockReturnValueOnce({
      hunks: [
        {
          oldStart: 1,
          oldLines: 600,
          newStart: 1,
          newLines: 600,
          lines: Array.from({ length: 600 }, (_, index) => ` line ${index}`),
        },
      ],
    });

    const results = synthesizeCollapsedEditDiffs(
      [file([{ old: "before", new: "after" }])],
      25,
      now,
    );

    expect(results).toEqual([undefined]);
  });
});

describe("expanded diff coordination", () => {
  it("runs queued files sequentially and gives later work only the row's remaining time", async () => {
    let now = 0;
    let finishFirst: ((value: FileDiff) => void) | undefined;
    const coordinator = createExpandedDiffCoordinator(5_000, () => now);
    const starts: Array<[string, number]> = [];
    const first = coordinator.run(
      (timeoutMs) =>
        new Promise<FileDiff>((resolve) => {
          starts.push(["first", timeoutMs]);
          finishFirst = resolve;
        }),
    );
    const second = coordinator.run(async (timeoutMs) => {
      starts.push(["second", timeoutMs]);
      return fileDiff([1]);
    });

    await Promise.resolve();
    expect(starts).toEqual([["first", 5_000]]);
    now = 125;
    finishFirst?.(fileDiff([1]));
    await first;
    await second;

    expect(starts).toEqual([
      ["first", 5_000],
      ["second", 4_875],
    ]);
  });

  it("does not start queued work after the row deadline expires", async () => {
    let now = 0;
    let finishFirst: ((value: FileDiff) => void) | undefined;
    const coordinator = createExpandedDiffCoordinator(5_000, () => now);
    const first = coordinator.run(
      () =>
        new Promise<FileDiff>((resolve) => {
          finishFirst = resolve;
        }),
    );
    const secondTask = vi.fn(async () => fileDiff([1]));
    const second = coordinator.run(secondTask);

    await Promise.resolve();
    now = 5_001;
    finishFirst?.(fileDiff([1]));
    await first;

    await expect(second).resolves.toBeUndefined();
    expect(secondTask).not.toHaveBeenCalled();
  });

  it("cancels queued work without affecting the result already in flight", async () => {
    let finishFirst: ((value: FileDiff) => void) | undefined;
    const coordinator = createExpandedDiffCoordinator();
    const first = coordinator.run(
      () =>
        new Promise<FileDiff>((resolve) => {
          finishFirst = resolve;
        }),
    );
    const secondTask = vi.fn(async () => fileDiff([1]));
    const second = coordinator.run(secondTask);

    await Promise.resolve();
    coordinator.cancel();
    finishFirst?.(fileDiff([1]));
    await expect(first).resolves.toBeDefined();
    await expect(second).resolves.toBeUndefined();
    expect(secondTask).not.toHaveBeenCalled();
  });

  it("skips one aborted queued job and still starts the current job behind it", async () => {
    let finishFirst: ((value: FileDiff) => void) | undefined;
    const coordinator = createExpandedDiffCoordinator();
    const first = coordinator.run(
      () =>
        new Promise<FileDiff>((resolve) => {
          finishFirst = resolve;
        }),
    );
    const obsoleteController = new AbortController();
    const obsoleteTask = vi.fn(async () => fileDiff([1]));
    const obsolete = coordinator.run(obsoleteTask, obsoleteController.signal);
    const currentTask = vi.fn(async () => fileDiff([1]));
    const current = coordinator.run(currentTask);

    await Promise.resolve();
    obsoleteController.abort();
    finishFirst?.(fileDiff([1]));
    await first;

    await expect(obsolete).resolves.toBeUndefined();
    await expect(current).resolves.toBeDefined();
    expect(obsoleteTask).not.toHaveBeenCalled();
    expect(currentTask).toHaveBeenCalledOnce();
  });
});

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

describe("MCP text mutation diffs", () => {
  it("renders snippet-relative removals and additions without a filesystem path", () => {
    const diff = synthesizeMcpTextEditDiff(
      "# Template\nHello {{ name }}\nNo trailing newline",
      "# Template\nHello {{ user }}\nNo trailing newline",
      false,
    );

    expect(diff.path).toBe("");
    expect(diff.truncated).toBe(false);
    expect(diff.hunks[0]!.header).toContain("snippet-relative");
    expect(diff.hunks.flatMap((hunk) => hunk.lines)).toEqual(
      expect.arrayContaining([
        expect.objectContaining({ origin: "removed", content: "Hello {{ name }}" }),
        expect.objectContaining({ origin: "added", content: "Hello {{ user }}" }),
      ]),
    );
  });

  it("forwards only content truncation and creates all-added Markdown content", () => {
    const edit = synthesizeMcpTextEditDiff("before", "after", true);
    expect(edit.truncated).toBe(true);

    const { diff, hiddenLines } = synthesizeMcpTextCreationDiff(
      "# Prompt\n\nUse {{ context }}\n",
      false,
    );
    expect(diff.path).toBe("");
    expect(diff.truncated).toBe(false);
    expect(hiddenLines).toBe(0);
    expect(diff.hunks.flatMap((hunk) => hunk.lines).every((line) => line.origin === "added")).toBe(
      true,
    );
  });
});
