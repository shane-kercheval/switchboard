import { afterEach, describe, expect, it } from "vitest";
import type { PaneLayout } from "./transcriptPanes.svelte";

const {
  layoutFor,
  reconcileLayout,
  hiddenCount,
  isAgentHidden,
  paneOfAgent,
  unassignedAgentIds,
  paneToCycleTo,
  toggleAgentHidden,
  soloAgent,
  showAllAgents,
  showAllInPane,
  createEmptyPane,
  moveAgentToPane,
  moveAgentToNewPane,
  unassignAgentFromPane,
  closePane,
  minimizePane,
  restorePane,
  maximizePane,
  restoreMaximizedPane,
  expandAllPanes,
  revealPane,
  renamePane,
  setFractions,
  setPaneRowWidth,
  _testing,
} = await import("./transcriptPanes.svelte");

const P = "project-1";
const ROSTER = ["a", "b", "c"];

afterEach(() => {
  _testing.reset();
});

/// Every roster agent appears in at most one pane; hidden entries must belong
/// to their pane. Unassigned agents are allowed.
function assertOptionalMembership(layout: PaneLayout, rosterIds: string[]): void {
  const seen = new Map<string, number>();
  for (const pane of layout.panes) {
    for (const id of pane.members) seen.set(id, (seen.get(id) ?? 0) + 1);
    for (const id of pane.hidden) expect(pane.members).toContain(id);
  }
  for (const id of seen.keys()) expect(rosterIds).toContain(id);
  expect([...seen.values()].every((n) => n === 1)).toBe(true);
  expect(layout.fractions).toHaveLength(layout.panes.length);
  expect(layout.fractions.reduce((acc, f) => acc + f, 0)).toBeCloseTo(1);
  expect(layout.minimized.every((id) => layout.panes.some((pane) => pane.id === id))).toBe(true);
  expect(
    layout.maximized === null || layout.panes.some((pane) => pane.id === layout.maximized),
  ).toBe(true);
}

describe("default layout", () => {
  it("starts as one pane named Pane 1 holding the whole roster", () => {
    const layout = layoutFor(P, ROSTER);
    expect(layout.panes).toHaveLength(1);
    expect(layout.panes[0]!.name).toBe("Pane 1");
    expect(layout.panes[0]!.members).toEqual(ROSTER);
    expect(layout.fractions).toEqual([1]);
    expect(layout.minimized).toEqual([]);
    expect(layout.maximized).toBeNull();
    assertOptionalMembership(layout, ROSTER);
  });

  it("scopes layout to its project", () => {
    moveAgentToNewPane(P, ROSTER, "b");
    expect(layoutFor(P, ROSTER).panes).toHaveLength(2);
    expect(layoutFor("project-2", ROSTER).panes).toHaveLength(1);
  });
});

describe("visibility (eye / solo / show all)", () => {
  it("toggles an agent hidden and back", () => {
    toggleAgentHidden(P, ROSTER, "b");
    expect(isAgentHidden(P, ROSTER, "b")).toBe(true);
    expect(hiddenCount(P, ROSTER)).toBe(1);
    toggleAgentHidden(P, ROSTER, "b");
    expect(isAgentHidden(P, ROSTER, "b")).toBe(false);
    expect(hiddenCount(P, ROSTER)).toBe(0);
  });

  it("solo hides every other member of the agent's pane", () => {
    soloAgent(P, ROSTER, "a");
    expect(isAgentHidden(P, ROSTER, "a")).toBe(false);
    expect(isAgentHidden(P, ROSTER, "b")).toBe(true);
    expect(isAgentHidden(P, ROSTER, "c")).toBe(true);
  });

  it("re-soloing the soloed agent restores the pane", () => {
    soloAgent(P, ROSTER, "a");
    soloAgent(P, ROSTER, "a");
    expect(hiddenCount(P, ROSTER)).toBe(0);
  });

  it("soloing a different agent re-targets the solo", () => {
    soloAgent(P, ROSTER, "a");
    soloAgent(P, ROSTER, "b");
    expect(isAgentHidden(P, ROSTER, "b")).toBe(false);
    expect(isAgentHidden(P, ROSTER, "a")).toBe(true);
    expect(isAgentHidden(P, ROSTER, "c")).toBe(true);
  });

  it("solo is pane-local: other panes' visibility is untouched", () => {
    moveAgentToNewPane(P, ROSTER, "c");
    soloAgent(P, ROSTER, "a"); // pane 1 holds a,b — solo a hides only b
    expect(isAgentHidden(P, ROSTER, "b")).toBe(true);
    expect(isAgentHidden(P, ROSTER, "c")).toBe(false);
  });

  it("show all clears hidden across every pane", () => {
    moveAgentToNewPane(P, ROSTER, "c");
    toggleAgentHidden(P, ROSTER, "a");
    toggleAgentHidden(P, ROSTER, "c");
    expect(hiddenCount(P, ROSTER)).toBe(2);
    showAllAgents(P, ROSTER);
    expect(hiddenCount(P, ROSTER)).toBe(0);
  });

  it("showAllInPane clears only the named pane's hidden set", () => {
    const p2 = moveAgentToNewPane(P, ROSTER, "c");
    toggleAgentHidden(P, ROSTER, "a"); // pane 1
    toggleAgentHidden(P, ROSTER, "c"); // pane 2
    showAllInPane(P, ROSTER, p2);
    expect(isAgentHidden(P, ROSTER, "c")).toBe(false);
    expect(isAgentHidden(P, ROSTER, "a")).toBe(true);
  });
});

describe("membership (move / new pane / unassign)", () => {
  it("move to a new pane removes the agent from its old pane", () => {
    const paneId = moveAgentToNewPane(P, ROSTER, "b");
    const layout = layoutFor(P, ROSTER);
    expect(layout.panes).toHaveLength(2);
    expect(layout.panes[0]!.members).toEqual(["a", "c"]);
    expect(layout.panes[1]!.id).toBe(paneId);
    expect(layout.panes[1]!.members).toEqual(["b"]);
    assertOptionalMembership(layout, ROSTER);
  });

  it("move to an existing pane never duplicates", () => {
    const paneId = moveAgentToNewPane(P, ROSTER, "b");
    moveAgentToPane(P, ROSTER, "c", paneId);
    const layout = layoutFor(P, ROSTER);
    expect(layout.panes[0]!.members).toEqual(["a"]);
    expect(layout.panes[1]!.members).toEqual(["b", "c"]);
    assertOptionalMembership(layout, ROSTER);
  });

  it("moving an agent drops its hidden entry from the old pane", () => {
    toggleAgentHidden(P, ROSTER, "b");
    moveAgentToNewPane(P, ROSTER, "b");
    expect(isAgentHidden(P, ROSTER, "b")).toBe(false);
    assertOptionalMembership(layoutFor(P, ROSTER), ROSTER);
  });

  it("optional membership holds after an arbitrary op sequence", () => {
    const p2 = moveAgentToNewPane(P, ROSTER, "b");
    const p3 = moveAgentToNewPane(P, ROSTER, "c");
    moveAgentToPane(P, ROSTER, "a", p2);
    moveAgentToPane(P, ROSTER, "b", p3);
    closePane(P, ROSTER, p2);
    const layout = layoutFor(P, ROSTER);
    assertOptionalMembership(layout, ROSTER);
  });

  it("new panes get unique default names, skipping renamed collisions", () => {
    const p2 = moveAgentToNewPane(P, ROSTER, "b");
    expect(layoutFor(P, ROSTER).panes[1]!.name).toBe("Pane 2");
    renamePane(P, ROSTER, p2, "Pane 3");
    moveAgentToNewPane(P, ROSTER, "c");
    const names = layoutFor(P, ROSTER).panes.map((p) => p.name);
    expect(new Set(names).size).toBe(names.length);
  });

  it("creates an empty pane at the right edge", () => {
    const paneId = createEmptyPane(P, ROSTER);
    const layout = layoutFor(P, ROSTER);
    expect(layout.panes).toHaveLength(2);
    expect(layout.panes[1]!.id).toBe(paneId);
    expect(layout.panes[1]!.name).toBe("Pane 2");
    expect(layout.panes[1]!.members).toEqual([]);
    expect(layout.panes[0]!.members).toEqual(ROSTER);
    assertOptionalMembership(layout, ROSTER);
  });

  it("starts an empty pane minimized when the row cannot fit another expanded pane", () => {
    setPaneRowWidth(800);
    moveAgentToNewPane(P, ROSTER, "b");
    const paneId = createEmptyPane(P, ROSTER);
    expect(layoutFor(P, ROSTER).minimized).toEqual([paneId]);
  });

  it("does not count minimized panes when deciding whether a new pane fits", () => {
    setPaneRowWidth(1200);
    const p2 = moveAgentToNewPane(P, ROSTER, "b");
    const p3 = moveAgentToNewPane(P, ROSTER, "c");
    minimizePane(P, ROSTER, p2);
    minimizePane(P, ROSTER, p3);

    const p4 = createEmptyPane(P, ROSTER);

    const layout = layoutFor(P, ROSTER);
    expect(layout.minimized).toEqual([p2, p3]);
    expect(layout.minimized).not.toContain(p4);
  });

  it("an emptied pane stays open", () => {
    const p2 = moveAgentToNewPane(P, ROSTER, "b");
    moveAgentToPane(P, ROSTER, "b", layoutFor(P, ROSTER).panes[0]!.id);
    const layout = layoutFor(P, ROSTER);
    expect(layout.panes).toHaveLength(2);
    expect(layout.panes.find((p) => p.id === p2)!.members).toEqual([]);
    assertOptionalMembership(layout, ROSTER);
  });

  it("paneOfAgent reports the hosting pane", () => {
    const p2 = moveAgentToNewPane(P, ROSTER, "b");
    expect(paneOfAgent(P, ROSTER, "b")!.id).toBe(p2);
    expect(paneOfAgent(P, ROSTER, "a")!.name).toBe("Pane 1");
    expect(paneOfAgent(P, ROSTER, "nope")).toBeNull();
  });

  it("unassigns an agent from its pane", () => {
    unassignAgentFromPane(P, ROSTER, "b");
    const layout = layoutFor(P, ROSTER);
    expect(layout.panes[0]!.members).toEqual(["a", "c"]);
    expect(paneOfAgent(P, ROSTER, "b")).toBeNull();
    expect(unassignedAgentIds(P, ROSTER)).toEqual(["b"]);
    assertOptionalMembership(layout, ROSTER);
  });

  it("moving an unassigned agent assigns it to the target pane", () => {
    const p2 = moveAgentToNewPane(P, ROSTER, "c");
    unassignAgentFromPane(P, ROSTER, "b");
    moveAgentToPane(P, ROSTER, "b", p2);
    const layout = layoutFor(P, ROSTER);
    expect(layout.panes[1]!.members).toEqual(["c", "b"]);
    expect(unassignedAgentIds(P, ROSTER)).toEqual([]);
    assertOptionalMembership(layout, ROSTER);
  });
});

describe("close pane", () => {
  it("unassigns the closed pane's members", () => {
    const p2 = moveAgentToNewPane(P, ROSTER, "b");
    moveAgentToPane(P, ROSTER, "c", p2);
    toggleAgentHidden(P, ROSTER, "c");
    closePane(P, ROSTER, p2);
    const layout = layoutFor(P, ROSTER);
    expect(layout.panes).toHaveLength(1);
    expect(layout.panes[0]!.members).toEqual(["a"]);
    expect(layout.panes[0]!.hidden).toEqual([]);
    expect(unassignedAgentIds(P, ROSTER)).toEqual(["b", "c"]);
    assertOptionalMembership(layout, ROSTER);
  });

  it("closing the leftmost pane leaves its members unassigned", () => {
    moveAgentToNewPane(P, ROSTER, "b"); // pane1: a,c · pane2: b
    const first = layoutFor(P, ROSTER).panes[0]!;
    closePane(P, ROSTER, first.id);
    const layout = layoutFor(P, ROSTER);
    expect(layout.panes).toHaveLength(1);
    expect(layout.panes[0]!.name).toBe("Pane 2");
    expect(layout.panes[0]!.members).toEqual(["b"]);
    expect(unassignedAgentIds(P, ROSTER)).toEqual(["a", "c"]);
    assertOptionalMembership(layout, ROSTER);
  });

  it("is unavailable (no-op) with a single pane", () => {
    const only = layoutFor(P, ROSTER).panes[0]!;
    closePane(P, ROSTER, only.id);
    expect(layoutFor(P, ROSTER).panes).toHaveLength(1);
    assertOptionalMembership(layoutFor(P, ROSTER), ROSTER);
  });

  it("the neighbor absorbs the closed pane's width share", () => {
    const p2 = moveAgentToNewPane(P, ROSTER, "b");
    moveAgentToNewPane(P, ROSTER, "c");
    setFractions(P, ROSTER, [0.5, 0.3, 0.2]);
    closePane(P, ROSTER, p2);
    expect(layoutFor(P, ROSTER).fractions[0]).toBeCloseTo(0.8);
  });

  it("clears minimized and maximized view state for the closed pane", () => {
    const p2 = moveAgentToNewPane(P, ROSTER, "b");
    const p3 = moveAgentToNewPane(P, ROSTER, "c");
    minimizePane(P, ROSTER, p3);
    maximizePane(P, ROSTER, p2);
    closePane(P, ROSTER, p2);
    const layout = layoutFor(P, ROSTER);
    expect(layout.maximized).toBeNull();
    expect(layout.minimized).toEqual([p3]);
    closePane(P, ROSTER, layout.panes[0]!.id);
    expect(layoutFor(P, ROSTER).minimized).toEqual([]);
  });
});

describe("pane display state", () => {
  it("minimizes and restores a pane without changing membership or fractions", () => {
    const p2 = moveAgentToNewPane(P, ROSTER, "b");
    const before = layoutFor(P, ROSTER);
    minimizePane(P, ROSTER, p2);
    let layout = layoutFor(P, ROSTER);
    expect(layout.minimized).toEqual([p2]);
    expect(layout.panes.map((pane) => pane.members)).toEqual(
      before.panes.map((pane) => pane.members),
    );
    expect(layout.fractions).toEqual(before.fractions);

    restorePane(P, ROSTER, p2);
    layout = layoutFor(P, ROSTER);
    expect(layout.minimized).toEqual([]);
    expect(layout.maximized).toBeNull();
  });

  it("does not minimize the last expanded pane", () => {
    const p2 = moveAgentToNewPane(P, ROSTER, "b");
    const [p1] = layoutFor(P, ROSTER).panes;
    minimizePane(P, ROSTER, p2);
    minimizePane(P, ROSTER, p1!.id);
    expect(layoutFor(P, ROSTER).minimized).toEqual([p2]);
  });

  it("starts a new pane minimized when the row cannot fit another expanded pane", () => {
    setPaneRowWidth(800);
    moveAgentToNewPane(P, ROSTER, "b");
    const p3 = moveAgentToNewPane(P, ROSTER, "c");
    const layout = layoutFor(P, ROSTER);
    expect(layout.panes).toHaveLength(3);
    expect(layout.minimized).toEqual([p3]);
  });

  it("maximizes, switches maximized panes, and restores", () => {
    const p2 = moveAgentToNewPane(P, ROSTER, "b");
    const p1 = layoutFor(P, ROSTER).panes[0]!.id;
    maximizePane(P, ROSTER, p1);
    expect(layoutFor(P, ROSTER).maximized).toBe(p1);
    maximizePane(P, ROSTER, p2);
    expect(layoutFor(P, ROSTER).maximized).toBe(p2);
    restoreMaximizedPane(P, ROSTER);
    expect(layoutFor(P, ROSTER).maximized).toBeNull();
  });

  it("reveal restores a minimized pane when nothing is maximized", () => {
    const p2 = moveAgentToNewPane(P, ROSTER, "b");
    minimizePane(P, ROSTER, p2);
    revealPane(P, ROSTER, p2);
    const layout = layoutFor(P, ROSTER);
    expect(layout.minimized).toEqual([]);
    expect(layout.maximized).toBeNull();
  });

  it("reveal replaces the maximized pane and unminimizes the target", () => {
    const p2 = moveAgentToNewPane(P, ROSTER, "b");
    const p1 = layoutFor(P, ROSTER).panes[0]!.id;
    minimizePane(P, ROSTER, p2);
    maximizePane(P, ROSTER, p1);
    revealPane(P, ROSTER, p2);
    const layout = layoutFor(P, ROSTER);
    // Focus mode is preserved (the target takes the maximized slot), and the
    // target leaves the minimized set so it doesn't vanish back to a tab when
    // maximization is later restored.
    expect(layout.maximized).toBe(p2);
    expect(layout.minimized).toEqual([]);
  });

  it("reveal replaces the maximized pane even when the target is not minimized", () => {
    const p2 = moveAgentToNewPane(P, ROSTER, "b");
    const p1 = layoutFor(P, ROSTER).panes[0]!.id;
    maximizePane(P, ROSTER, p1);
    revealPane(P, ROSTER, p2);
    expect(layoutFor(P, ROSTER).maximized).toBe(p2);
  });

  it("reveal is a no-op for an already-visible pane when nothing is maximized, and for an unknown pane", () => {
    const p2 = moveAgentToNewPane(P, ROSTER, "b");
    const before = layoutFor(P, ROSTER);
    revealPane(P, ROSTER, p2);
    expect(layoutFor(P, ROSTER)).toEqual(before);
    revealPane(P, ROSTER, "nonexistent-pane");
    expect(layoutFor(P, ROSTER)).toEqual(before);
  });

  it("restore from maximized never leaves every pane minimized", () => {
    const layout = reconcileLayout(
      {
        panes: [
          { id: "p1", name: "Pane 1", members: ["a"], hidden: [] },
          { id: "p2", name: "Pane 2", members: ["b"], hidden: [] },
        ],
        fractions: [0.5, 0.5],
        minimized: ["p1", "p2"],
        maximized: "p1",
      },
      ROSTER,
    );
    expect(layout.minimized).toEqual(["p1", "p2"]);
    const restored = reconcileLayout({ ...layout, maximized: null }, ROSTER);
    expect(restored.minimized).toHaveLength(1);
  });
});

describe("roster reconciliation", () => {
  it("prunes removed agents from members and hidden", () => {
    toggleAgentHidden(P, ROSTER, "c");
    const layout = layoutFor(P, ["a", "b"]);
    expect(layout.panes[0]!.members).toEqual(["a", "b"]);
    expect(layout.panes[0]!.hidden).toEqual([]);
    assertOptionalMembership(layout, ["a", "b"]);
  });

  it("leaves roster agents missing from every pane unassigned", () => {
    moveAgentToNewPane(P, ROSTER, "b");
    const layout = layoutFor(P, [...ROSTER, "d"]);
    expect(layout.panes[0]!.members).toEqual(["a", "c"]);
    expect(unassignedAgentIds(P, [...ROSTER, "d"])).toEqual(["d"]);
    assertOptionalMembership(layout, [...ROSTER, "d"]);
  });

  it("a duplicated agent keeps only its leftmost slot", () => {
    const layout = reconcileLayout(
      {
        panes: [
          { id: "p1", name: "Pane 1", members: ["a", "b"], hidden: [] },
          { id: "p2", name: "Pane 2", members: ["b", "c"], hidden: ["b"] },
        ],
        fractions: [0.5, 0.5],
        minimized: [],
        maximized: null,
      },
      ROSTER,
    );
    expect(layout.panes[0]!.members).toEqual(["a", "b"]);
    expect(layout.panes[1]!.members).toEqual(["c"]);
    expect(layout.panes[1]!.hidden).toEqual([]);
    assertOptionalMembership(layout, ROSTER);
  });

  it("normalizes malformed fractions to equal shares", () => {
    const layout = reconcileLayout(
      {
        panes: [
          { id: "p1", name: "Pane 1", members: ["a"], hidden: [] },
          { id: "p2", name: "Pane 2", members: ["b", "c"], hidden: [] },
        ],
        fractions: [0.5],
        minimized: ["p2"],
        maximized: "p2",
      },
      ROSTER,
    );
    expect(layout.fractions).toEqual([0.5, 0.5]);
    expect(layout.minimized).toEqual(["p2"]);
    expect(layout.maximized).toBe("p2");
  });

  it("prunes stale minimized and maximized pane ids", () => {
    const layout = reconcileLayout(
      {
        panes: [
          { id: "p1", name: "Pane 1", members: ["a"], hidden: [] },
          { id: "p2", name: "Pane 2", members: ["b"], hidden: [] },
        ],
        fractions: [0.5, 0.5],
        minimized: ["missing", "p2", "p2"],
        maximized: "missing",
      },
      ROSTER,
    );
    expect(layout.minimized).toEqual(["p2"]);
    expect(layout.maximized).toBeNull();
  });
});

describe("rename", () => {
  it("renames a pane, trimming whitespace", () => {
    const p2 = moveAgentToNewPane(P, ROSTER, "b");
    renamePane(P, ROSTER, p2, "  reviewers ");
    expect(layoutFor(P, ROSTER).panes[1]!.name).toBe("reviewers");
  });

  it("ignores an empty rename", () => {
    const p2 = moveAgentToNewPane(P, ROSTER, "b");
    renamePane(P, ROSTER, p2, "   ");
    expect(layoutFor(P, ROSTER).panes[1]!.name).toBe("Pane 2");
  });
});

describe("persistence", () => {
  it("round-trips panes, membership, hidden sets, and fractions", () => {
    const p2 = moveAgentToNewPane(P, ROSTER, "b");
    renamePane(P, ROSTER, p2, "reviewers");
    toggleAgentHidden(P, ROSTER, "a");
    setFractions(P, ROSTER, [0.7, 0.3]);
    _testing.reloadFromStorage();
    const layout = layoutFor(P, ROSTER);
    expect(layout.panes.map((p) => p.name)).toEqual(["Pane 1", "reviewers"]);
    expect(layout.panes[1]!.members).toEqual(["b"]);
    expect(layout.panes[0]!.hidden).toEqual(["a"]);
    expect(layout.fractions[0]).toBeCloseTo(0.7);
  });

  it("round-trips minimized and maximized pane state", () => {
    const p2 = moveAgentToNewPane(P, ROSTER, "b");
    minimizePane(P, ROSTER, p2);
    maximizePane(P, ROSTER, layoutFor(P, ROSTER).panes[0]!.id);
    _testing.reloadFromStorage();
    const layout = layoutFor(P, ROSTER);
    expect(layout.minimized).toEqual([p2]);
    expect(layout.maximized).toBe(layout.panes[0]!.id);
  });

  it("leaves agents missing from a stale stored layout unassigned on restore", () => {
    moveAgentToNewPane(P, ROSTER, "b");
    _testing.reloadFromStorage();
    const layout = layoutFor(P, [...ROSTER, "d"]);
    expect(layout.panes[0]!.members).not.toContain("d");
    expect(unassignedAgentIds(P, [...ROSTER, "d"])).toEqual(["d"]);
    assertOptionalMembership(layout, [...ROSTER, "d"]);
  });

  it("falls back to the default single pane on a corrupt entry", () => {
    localStorage.setItem("switchboard-transcript-panes", "{not json");
    _testing.reloadFromStorage();
    const layout = layoutFor(P, ROSTER);
    expect(layout.panes).toHaveLength(1);
    assertOptionalMembership(layout, ROSTER);
  });

  it("ignores an envelope with an unknown version", () => {
    localStorage.setItem(
      "switchboard-transcript-panes",
      JSON.stringify({ version: 999, projects: { [P]: { panes: [], fractions: [] } } }),
    );
    _testing.reloadFromStorage();
    expect(layoutFor(P, ROSTER).panes).toHaveLength(1);
  });

  it("drops a malformed project entry while keeping the rest", () => {
    moveAgentToNewPane(P, ROSTER, "b");
    const raw = JSON.parse(localStorage.getItem("switchboard-transcript-panes")!) as {
      version: number;
      projects: Record<string, unknown>;
    };
    raw.projects["bad-project"] = { panes: [{ nope: true }], fractions: [] };
    localStorage.setItem("switchboard-transcript-panes", JSON.stringify(raw));
    _testing.reloadFromStorage();
    expect(layoutFor(P, ROSTER).panes).toHaveLength(2);
    expect(layoutFor("bad-project", ROSTER).panes).toHaveLength(1);
  });
});

describe("fractions", () => {
  it("setFractions normalizes to sum 1", () => {
    moveAgentToNewPane(P, ROSTER, "b");
    setFractions(P, ROSTER, [3, 1]);
    const layout = layoutFor(P, ROSTER);
    expect(layout.fractions[0]).toBeCloseTo(0.75);
    expect(layout.fractions[1]).toBeCloseTo(0.25);
  });

  it("rejects a wrong-length or degenerate fraction list", () => {
    moveAgentToNewPane(P, ROSTER, "b");
    setFractions(P, ROSTER, [1]);
    expect(layoutFor(P, ROSTER).fractions).toEqual([0.5, 0.5]);
    setFractions(P, ROSTER, [0, 0]);
    expect(layoutFor(P, ROSTER).fractions).toEqual([0.5, 0.5]);
  });
});

describe("paneToCycleTo (positional pane cycling)", () => {
  // Pull b and c into their own panes → pane order [a] [b] [c].
  function threePanes(): void {
    moveAgentToNewPane(P, ROSTER, "b");
    moveAgentToNewPane(P, ROSTER, "c");
  }

  it("cycles to the next pane by position, wrapping at the end", () => {
    threePanes();
    expect(paneToCycleTo(P, ROSTER, ["b"], 1)?.members).toEqual(["c"]);
    expect(paneToCycleTo(P, ROSTER, ["c"], 1)?.members).toEqual(["a"]);
  });

  it("cycles to the previous pane by position, wrapping at the start", () => {
    threePanes();
    expect(paneToCycleTo(P, ROSTER, ["b"], -1)?.members).toEqual(["a"]);
    expect(paneToCycleTo(P, ROSTER, ["a"], -1)?.members).toEqual(["c"]);
  });

  it("enters from the near end when the selection matches no single pane", () => {
    threePanes();
    expect(paneToCycleTo(P, ROSTER, [], 1)?.members).toEqual(["a"]); // next → leftmost
    expect(paneToCycleTo(P, ROSTER, [], -1)?.members).toEqual(["c"]); // prev → rightmost
    expect(paneToCycleTo(P, ROSTER, ["a", "b"], 1)?.members).toEqual(["a"]); // cross-pane custom set
  });

  it("treats the maximized pane as the current one, ignoring the selection", () => {
    threePanes();
    const p2 = layoutFor(P, ROSTER).panes[1]!.id;
    maximizePane(P, ROSTER, p2);
    // Selection points at pane 1, but pane 2 is maximized → next is pane 3.
    expect(paneToCycleTo(P, ROSTER, ["a"], 1)?.members).toEqual(["c"]);
  });

  it("skips empty panes", () => {
    threePanes();
    createEmptyPane(P, ROSTER); // a 4th, member-less pane
    expect(paneToCycleTo(P, ROSTER, ["c"], 1)?.members).toEqual(["a"]); // wraps past the empty one
  });

  it("returns null when fewer than two panes can be cycled", () => {
    expect(paneToCycleTo(P, ROSTER, ["a"], 1)).toBeNull(); // default single pane
    createEmptyPane(P, ROSTER);
    expect(paneToCycleTo(P, ROSTER, ["a"], 1)).toBeNull(); // one real pane + one empty
  });
});

describe("expandAllPanes", () => {
  it("clears both the maximized pane and all minimized panes", () => {
    moveAgentToNewPane(P, ROSTER, "b");
    moveAgentToNewPane(P, ROSTER, "c"); // 3 panes: [a] [b] [c]
    const initial = layoutFor(P, ROSTER).panes;
    minimizePane(P, ROSTER, initial[1]!.id);
    maximizePane(P, ROSTER, initial[0]!.id);
    expect(layoutFor(P, ROSTER).maximized).not.toBeNull();
    expect(layoutFor(P, ROSTER).minimized.length).toBeGreaterThan(0);

    expandAllPanes(P, ROSTER);

    const layout = layoutFor(P, ROSTER);
    expect(layout.maximized).toBeNull();
    expect(layout.minimized).toEqual([]);
  });
});
