import { describe, expect, it } from "vitest";
import { localBranchBadges, remoteBranchBadges, badgeToneClass } from "./gitBadges";
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

const keys = (badges: { key: string }[]): string[] => badges.map((b) => b.key);

describe("localBranchBadges", () => {
  it("clean + in-sync branch shows NO badge (the calm default)", () => {
    expect(localBranchBadges(branch({ sync: { kind: "in_sync" }, merged: false }), null)).toEqual(
      [],
    );
  });

  it("uncommitted changes (dirty OR untracked) collapse to one warning badge", () => {
    const dirty = localBranchBadges(branch({ worktree: worktree({ dirty: true }) }), null);
    expect(keys(dirty)).toEqual(["uncommitted"]);
    expect(dirty[0]!.tone).toBe("warning");

    const untracked = localBranchBadges(branch({ worktree: worktree({ untracked: true }) }), null);
    expect(keys(untracked)).toEqual(["uncommitted"]);
  });

  it("behind-base is an amber 'out of date' badge", () => {
    const b = localBranchBadges(branch({ behind_base: 3 }), null);
    expect(keys(b)).toContain("behind_base");
    expect(b.find((x) => x.key === "behind_base")!.tone).toBe("warning");
    // behind_base of 0 is not stale.
    expect(keys(localBranchBadges(branch({ behind_base: 0 }), null))).not.toContain("behind_base");
  });

  it("ahead/behind/diverged are neutral count chips, not warnings", () => {
    const ahead = localBranchBadges(branch({ sync: { kind: "ahead", commits: 2 } }), null);
    expect(ahead[0]!.tone).toBe("neutral");
    expect(ahead[0]!.label).toBe("↑2");

    const diverged: SyncState = { kind: "diverged", ahead: 1, behind: 4 };
    const d = localBranchBadges(branch({ sync: diverged }), null);
    expect(d[0]!.tone).toBe("neutral");
    expect(d[0]!.label).toBe("↑1 ↓4");
  });

  it("local-only is a muted fact, merged is a muted label", () => {
    expect(localBranchBadges(branch({ sync: { kind: "local_only" } }), null)[0]).toMatchObject({
      key: "local_only",
      tone: "muted",
    });
    expect(
      localBranchBadges(branch({ merged: true }), null).find((b) => b.key === "merged")!.tone,
    ).toBe("muted");
  });

  it("suppresses the merged badge on the default branch itself", () => {
    // `main` is trivially an ancestor of its own tip, so M1 reports it merged —
    // but labeling the default branch "safe to delete" is exactly wrong.
    expect(keys(localBranchBadges(branch({ name: "main", merged: true }), "main"))).not.toContain(
      "merged",
    );
    // A non-default merged branch still shows it.
    expect(keys(localBranchBadges(branch({ name: "feature", merged: true }), "main"))).toContain(
      "merged",
    );
  });

  it("dangling and orphaned/prunable worktree warnings are amber", () => {
    expect(
      localBranchBadges(branch({ dangling: true }), null).find((b) => b.key === "dangling")!.tone,
    ).toBe("warning");
    const orphaned = localBranchBadges(
      branch({ worktree: worktree({ warning: "orphaned" }) }),
      null,
    );
    expect(keys(orphaned)).toContain("orphaned");
  });
});

describe("remoteBranchBadges", () => {
  it("carries only the two cleanup signals (merged, behind_base)", () => {
    const b = (over: Partial<RemoteBranchView> = {}): RemoteBranchView => ({
      name: "origin/feature",
      merged: null,
      behind_base: null,
      ...over,
    });
    expect(remoteBranchBadges(b(), null)).toEqual([]);
    expect(keys(remoteBranchBadges(b({ behind_base: 5 }), null))).toEqual(["behind_base"]);
    expect(keys(remoteBranchBadges(b({ merged: true }), null))).toEqual(["merged"]);
  });

  it("suppresses merged on the default branch's own remote ref (origin/<default>)", () => {
    const b = (over: Partial<RemoteBranchView> = {}): RemoteBranchView => ({
      name: "origin/main",
      merged: true,
      behind_base: null,
      ...over,
    });
    expect(keys(remoteBranchBadges(b(), "main"))).not.toContain("merged");
    // A non-default remote branch still shows merged.
    expect(keys(remoteBranchBadges(b({ name: "origin/feature" }), "main"))).toContain("merged");
  });
});

describe("badgeToneClass", () => {
  it("warning maps to the amber attention token; no new hue", () => {
    expect(badgeToneClass("warning")).toContain("warning");
    expect(badgeToneClass("neutral")).toContain("panel");
    expect(badgeToneClass("muted")).toContain("muted");
  });
});
