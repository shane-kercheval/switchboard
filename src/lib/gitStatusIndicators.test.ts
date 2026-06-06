import { describe, expect, it } from "vitest";
import {
  indicatorToneClass,
  localBranchIndicators,
  remoteBranchIndicators,
} from "./gitStatusIndicators";
import type { BranchView, RemoteBranchView, SyncState, WorktreeView } from "./types";

const branch = (over: Partial<BranchView> = {}): BranchView => ({
  name: "feature",
  upstream: null,
  sync: { kind: "in_sync" },
  behind_base: null,
  merged: null,
  dangling: false,
  worktree: null,
  ...over,
});

const worktree = (over: Partial<WorktreeView> = {}): WorktreeView => ({
  path: "/repo",
  dirty: false,
  untracked: false,
  detached_hash: null,
  warning: null,
  ...over,
});

const keys = (indicators: { key: string }[]): string[] => indicators.map((b) => b.key);

describe("localBranchIndicators", () => {
  it("clean + in-sync branch shows NO indicator (the calm default)", () => {
    expect(
      localBranchIndicators(branch({ sync: { kind: "in_sync" }, merged: false }), null),
    ).toEqual([]);
  });

  it("uncommitted changes (dirty OR untracked) collapse to one warning indicator", () => {
    const dirty = localBranchIndicators(branch({ worktree: worktree({ dirty: true }) }), null);
    expect(keys(dirty)).toEqual(["uncommitted"]);
    expect(dirty[0]!.tone).toBe("warning");

    const untracked = localBranchIndicators(
      branch({ worktree: worktree({ untracked: true }) }),
      null,
    );
    expect(keys(untracked)).toEqual(["uncommitted"]);
  });

  it("behind-base is an attention indicator", () => {
    const b = localBranchIndicators(branch({ behind_base: 3 }), null);
    expect(keys(b)).toContain("behind_base");
    expect(b.find((x) => x.key === "behind_base")!.tone).toBe("warning");
    expect(keys(localBranchIndicators(branch({ behind_base: 0 }), null))).not.toContain(
      "behind_base",
    );
  });

  it("ahead/behind/diverged are neutral count indicators, not warnings", () => {
    const ahead = localBranchIndicators(branch({ sync: { kind: "ahead", commits: 2 } }), null);
    expect(ahead[0]!.tone).toBe("neutral");
    expect(ahead[0]!.label).toBe("↑2");

    const diverged: SyncState = { kind: "diverged", ahead: 1, behind: 4 };
    const d = localBranchIndicators(branch({ sync: diverged }), null);
    expect(d[0]!.tone).toBe("neutral");
    expect(d[0]!.label).toBe("↑1 ↓4");
  });

  it("local-only is a muted fact, merged is a muted label", () => {
    expect(localBranchIndicators(branch({ sync: { kind: "local_only" } }), null)[0]).toMatchObject({
      key: "local_only",
      tone: "muted",
    });
    expect(
      localBranchIndicators(branch({ merged: true }), null).find((b) => b.key === "merged")!.tone,
    ).toBe("muted");
  });

  it("suppresses the merged indicator on the default branch itself", () => {
    expect(
      keys(localBranchIndicators(branch({ name: "main", merged: true }), "main")),
    ).not.toContain("merged");
    expect(
      keys(localBranchIndicators(branch({ name: "feature", merged: true }), "main")),
    ).toContain("merged");
  });

  it("dangling and orphaned/prunable worktree warnings are attention indicators", () => {
    expect(
      localBranchIndicators(branch({ dangling: true }), null).find((b) => b.key === "dangling")!
        .tone,
    ).toBe("warning");
    const orphaned = localBranchIndicators(
      branch({ worktree: worktree({ warning: "orphaned" }) }),
      null,
    );
    expect(keys(orphaned)).toContain("orphaned");
  });

  it("renders branch relationship/state before worktree and cleanup indicators", () => {
    expect(
      keys(
        localBranchIndicators(
          branch({
            upstream: "origin/feature",
            sync: { kind: "in_sync" },
            behind_base: 2,
            worktree: worktree({ dirty: true }),
          }),
          "main",
        ),
      ),
    ).toEqual(["upstream", "uncommitted", "behind_base"]);

    expect(
      keys(
        localBranchIndicators(
          branch({
            dangling: true,
            sync: { kind: "behind", commits: 1 },
            worktree: worktree({ dirty: true }),
          }),
          "main",
        ),
      )[0],
    ).toBe("dangling");
  });
});

describe("remoteBranchIndicators", () => {
  it("carries only the two cleanup signals (merged, behind_base)", () => {
    const b = (over: Partial<RemoteBranchView> = {}): RemoteBranchView => ({
      name: "origin/feature",
      merged: null,
      behind_base: null,
      ...over,
    });
    expect(remoteBranchIndicators(b(), null)).toEqual([]);
    expect(keys(remoteBranchIndicators(b({ behind_base: 5 }), null))).toEqual(["behind_base"]);
    expect(keys(remoteBranchIndicators(b({ merged: true }), null))).toEqual(["merged"]);
  });

  it("suppresses merged on the default branch's own remote ref (origin/<default>)", () => {
    const b = (over: Partial<RemoteBranchView> = {}): RemoteBranchView => ({
      name: "origin/main",
      merged: true,
      behind_base: null,
      ...over,
    });
    expect(keys(remoteBranchIndicators(b(), "main"))).not.toContain("merged");
    expect(keys(remoteBranchIndicators(b({ name: "origin/feature" }), "main"))).toContain("merged");
  });
});

describe("indicatorToneClass", () => {
  it("warning maps to attention color without adding a framed badge surface", () => {
    expect(indicatorToneClass("warning")).toContain("warning");
    expect(indicatorToneClass("neutral")).toContain("muted");
    expect(indicatorToneClass("muted")).toContain("muted");
  });
});
