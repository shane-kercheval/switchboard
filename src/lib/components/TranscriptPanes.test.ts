import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/svelte";
import type { AgentRecord, ConversationItem, NormalizedEvent } from "$lib/types";
// Static import so the component-tree transform happens at module collection,
// not inside the first test's timeout (cold CI transforms have no vite cache).
// `vi.mock` is hoisted above imports, so the mocks below still apply.
import TranscriptPanes from "./TranscriptPanes.svelte";
import {
  layoutFor,
  minimizePane,
  moveAgentToPane,
  moveAgentToNewPane,
  toggleAgentHidden,
  _testing as panesState,
} from "$lib/state/transcriptPanes.svelte";
import {
  selectionFor,
  setRecipients,
  _testing as selectionState,
} from "$lib/state/recipientSelection.svelte";
import { setProjectCompact, _testing as previewState } from "$lib/state/transcriptPreview.svelte";
import { shortcut } from "$lib/platform";

const listeners = new Map<string, (e: { payload: NormalizedEvent }) => void>();
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (name: string, cb: (e: { payload: NormalizedEvent }) => void) => {
    listeners.set(name, cb);
    return vi.fn();
  }),
}));

const invokeMock = vi.fn(
  async (_cmd: string, _args?: Record<string, unknown>): Promise<unknown> => null,
);
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: Record<string, unknown>) => invokeMock(cmd, args),
  convertFileSrc: (path: string) => `asset://localhost/${path}`,
}));

const copyTextMock = vi.fn(async (_t: string): Promise<void> => undefined);
vi.mock("$lib/native", () => ({
  copyText: (t: string) => copyTextMock(t),
}));

async function loadState() {
  return await import("$lib/state/index.svelte");
}

const PROJECT_ID = "00000000-0000-7000-8000-0000000000ff";

const ALICE: AgentRecord = {
  id: "00000000-0000-7000-8000-000000000aaa",
  project_id: PROJECT_ID,
  name: "alice",
  harness: "claude_code",
  session_locator: { uuid: "00000000-0000-7000-8000-000000000001" },
  created_at: "2026-05-16T00:00:00Z",
};
const BOB: AgentRecord = {
  id: "00000000-0000-7000-8000-000000000bbb",
  project_id: PROJECT_ID,
  name: "bob",
  harness: "codex",
  session_locator: null,
  created_at: "2026-05-16T00:00:01Z",
};
const ROSTER = [ALICE, BOB];
const ROSTER_IDS = [ALICE.id, BOB.id];

function numberedAgent(index: number): AgentRecord {
  return {
    id: `00000000-0000-7000-8000-${String(index).padStart(12, "0")}`,
    project_id: PROJECT_ID,
    name: `agent-${index}`,
    harness: index % 2 === 0 ? "codex" : "claude_code",
    session_locator: null,
    created_at: `2026-05-16T00:00:${String(index).padStart(2, "0")}Z`,
  };
}

async function seedTwoAgentTranscripts(): Promise<void> {
  const state = await loadState();
  await state.registerAgent(ALICE);
  await state.registerAgent(BOB);
  state.transcripts[ALICE.id] = [
    {
      role: "user",
      turn_id: "user-1",
      agent_id: ALICE.id,
      started_at: "2026-05-16T00:00:00Z",
      text: "hi alice",
    },
    {
      role: "agent",
      turn_id: "agent-1",
      agent_id: ALICE.id,
      started_at: "2026-05-16T00:00:01Z",
      status: "complete",
      items: [{ item_kind: "text", kind: "text", text: "hello from alice" }],
    },
  ];
  state.transcripts[BOB.id] = [
    {
      role: "user",
      turn_id: "user-2",
      agent_id: BOB.id,
      started_at: "2026-05-16T00:00:02Z",
      text: "hi bob",
    },
    {
      role: "agent",
      turn_id: "agent-2",
      agent_id: BOB.id,
      started_at: "2026-05-16T00:00:03Z",
      status: "complete",
      items: [{ item_kind: "text", kind: "text", text: "hello from bob" }],
    },
  ];
}

function renderPanes(overlay: ConversationItem[] = []) {
  return render(TranscriptPanes, {
    props: { projectId: PROJECT_ID, agents: ROSTER, overlay },
  });
}

function paneEls(): HTMLElement[] {
  return screen.getAllByTestId("transcript-pane");
}

beforeEach(() => {
  // Module-global pane/selection state resets up front (not in afterEach):
  // vitest afterEach hooks run LIFO, before testing-library's auto-cleanup
  // unmounts the previous test's component — resetting under a live component
  // lets its effects observe (and react to) the wipe.
  panesState.reset();
  selectionState.reset();
  listeners.clear();
  invokeMock.mockReset();
  setProjectCompact(PROJECT_ID, false);
});

afterEach(async () => {
  const { _testing } = await loadState();
  _testing.reset();
  previewState.reset();
});

describe("single pane (the no-split default)", () => {
  it("renders one pane with no chrome: no header, no gutter, no coverage, no overlay", async () => {
    await seedTwoAgentTranscripts();
    renderPanes();

    expect(paneEls()).toHaveLength(1);
    expect(screen.queryByTestId("pane-header")).not.toBeInTheDocument();
    expect(screen.queryByTestId("pane-gutter-1")).not.toBeInTheDocument();
    expect(paneEls()[0]).not.toHaveAttribute("data-coverage");
    expect(screen.queryByTestId("pane-coverage")).not.toBeInTheDocument();

    // Targeting chrome is inert with one pane: holding Cmd shows no overlay.
    await fireEvent.pointerEnter(paneEls()[0]!);
    await fireEvent.keyDown(window, { key: "Meta" });
    expect(screen.queryByTestId("pane-target-overlay")).not.toBeInTheDocument();

    // Cmd+click does not re-target either.
    setRecipients(PROJECT_ID, [ALICE.id]);
    await fireEvent.click(paneEls()[0]!, { metaKey: true });
    expect(selectionFor(PROJECT_ID)).toEqual([ALICE.id]);
  });

  it("shows the full merged transcript", async () => {
    await seedTwoAgentTranscripts();
    renderPanes();
    const turns = screen.getAllByTestId("turn");
    expect(turns).toHaveLength(4);
  });
});

describe("visibility filtering in the single default pane", () => {
  it("hiding an agent removes its turns and sole-recipient messages; mixed-recipient messages survive", async () => {
    await seedTwoAgentTranscripts();
    const overlay: ConversationItem[] = [
      {
        kind: "user_message",
        id: "send-both",
        send_id: "send-both",
        agent_ids: [ALICE.id, BOB.id],
        text: "to both agents",
        at: "2026-05-16T00:00:04Z",
      },
    ];
    toggleAgentHidden(PROJECT_ID, ROSTER_IDS, BOB.id);
    renderPanes(overlay);

    // Bob's turns and his sole-recipient prompt are gone...
    expect(screen.queryByText("hello from bob")).not.toBeInTheDocument();
    expect(screen.queryByText("hi bob")).not.toBeInTheDocument();
    // ...alice's remain, and the mixed-recipient message survives (pruned to
    // its surviving recipient).
    expect(screen.getByText("hello from alice")).toBeInTheDocument();
    expect(screen.getByText("to both agents")).toBeInTheDocument();
  });
});

describe("per-pane content (partition)", () => {
  it("each pane shows only its agents' turns and their user messages", async () => {
    await seedTwoAgentTranscripts();
    moveAgentToNewPane(PROJECT_ID, ROSTER_IDS, BOB.id);
    renderPanes();

    const [paneA, paneB] = paneEls();
    expect(paneEls()).toHaveLength(2);

    expect(within(paneA!).getByText("hi alice")).toBeInTheDocument();
    expect(within(paneA!).getByText("hello from alice")).toBeInTheDocument();
    expect(within(paneA!).queryByText("hi bob")).not.toBeInTheDocument();
    expect(within(paneA!).queryByText("hello from bob")).not.toBeInTheDocument();

    expect(within(paneB!).getByText("hi bob")).toBeInTheDocument();
    expect(within(paneB!).getByText("hello from bob")).toBeInTheDocument();
    expect(within(paneB!).queryByText("hi alice")).not.toBeInTheDocument();
  });

  it("a historical user message renders only in panes hosting a recipient", async () => {
    await seedTwoAgentTranscripts();
    moveAgentToNewPane(PROJECT_ID, ROSTER_IDS, BOB.id);
    const overlay: ConversationItem[] = [
      {
        kind: "user_message",
        id: "send-bob-only",
        send_id: "send-bob-only",
        agent_ids: [BOB.id],
        text: "for bob's eyes",
        at: "2026-05-16T00:00:04Z",
      },
    ];
    renderPanes(overlay);

    const [paneA, paneB] = paneEls();
    expect(within(paneB!).getByText("for bob's eyes")).toBeInTheDocument();
    expect(within(paneA!).queryByText("for bob's eyes")).not.toBeInTheDocument();
  });

  it("a fan-out spanning panes renders the user message in each pane, single-recipient style", async () => {
    const state = await loadState();
    await state.registerAgent(ALICE);
    await state.registerAgent(BOB);
    const sendId = "00000000-0000-7000-8000-0000000000d1";
    state.transcripts[ALICE.id] = [
      {
        role: "user",
        turn_id: "user-1",
        agent_id: ALICE.id,
        send_id: sendId,
        started_at: "2026-05-16T00:00:00Z",
        text: "both of you",
      },
    ];
    state.transcripts[BOB.id] = [
      {
        role: "user",
        turn_id: "user-2",
        agent_id: BOB.id,
        send_id: sendId,
        started_at: "2026-05-16T00:00:00Z",
        text: "both of you",
      },
    ];
    moveAgentToNewPane(PROJECT_ID, ROSTER_IDS, BOB.id);
    renderPanes();

    // Each pane's roster is one recipient, so buildUnifiedRows collapses the
    // fan-out to a plain single-recipient send per pane — message once per
    // pane, no fan-out columns.
    const [paneA, paneB] = paneEls();
    expect(within(paneA!).getAllByText("both of you")).toHaveLength(1);
    expect(within(paneB!).getAllByText("both of you")).toHaveLength(1);
    expect(screen.queryByTestId("fanout-group")).not.toBeInTheDocument();
  });
});

describe("pane chrome (headers, rename, close)", () => {
  it("renders headers with pane names once split", async () => {
    await seedTwoAgentTranscripts();
    moveAgentToNewPane(PROJECT_ID, ROSTER_IDS, BOB.id);
    renderPanes();
    const names = screen.getAllByTestId("pane-name").map((el) => el.textContent?.trim());
    expect(names).toEqual(["Pane 1", "Pane 2"]);
    const chips = screen.getAllByTestId("pane-member-chip").map((el) => el.textContent?.trim());
    expect(chips).toEqual(["alice", "bob"]);
  });

  it("renames via the explicit edit affordance without re-targeting", async () => {
    await seedTwoAgentTranscripts();
    moveAgentToNewPane(PROJECT_ID, ROSTER_IDS, BOB.id);
    setRecipients(PROJECT_ID, []);
    renderPanes();

    await fireEvent.click(screen.getAllByTestId("pane-actions")[0]!);
    await fireEvent.click(screen.getByTestId("pane-rename"));
    const input = screen.getByTestId("pane-rename-input");
    await fireEvent.input(input, { target: { value: "reviewers" } });
    await fireEvent.keyDown(input, { key: "Enter" });

    expect(screen.getAllByTestId("pane-name")[0]).toHaveTextContent("reviewers");
    // Entering rename mode / committing never touched the recipient set.
    expect(selectionFor(PROJECT_ID)).toEqual([]);
  });

  it("close unassigns the pane's agents", async () => {
    await seedTwoAgentTranscripts();
    moveAgentToNewPane(PROJECT_ID, ROSTER_IDS, BOB.id);
    renderPanes();

    await fireEvent.click(screen.getAllByTestId("pane-actions")[1]!);
    await fireEvent.click(screen.getByTestId("pane-close"));

    expect(paneEls()).toHaveLength(1);
    const layout = layoutFor(PROJECT_ID, ROSTER_IDS);
    expect(layout.panes[0]!.members).toEqual([ALICE.id]);
    expect(within(paneEls()[0]!).getByText("hello from alice")).toBeInTheDocument();
    expect(within(paneEls()[0]!).queryByText("hello from bob")).not.toBeInTheDocument();
    // One pane remains, but chrome stays visible while unassigned agents exist
    // so they can be added back from the pane header.
    expect(screen.getByTestId("pane-header")).toBeInTheDocument();
    expect(screen.getByTestId("pane-actions")).toBeInTheDocument();
  });

  it("removing a pane member chip unassigns that agent", async () => {
    await seedTwoAgentTranscripts();
    moveAgentToNewPane(PROJECT_ID, ROSTER_IDS, BOB.id);
    renderPanes();

    const paneB = paneEls()[1]!;
    await fireEvent.click(within(paneB).getByTestId("pane-member-remove"));

    const layout = layoutFor(PROJECT_ID, ROSTER_IDS);
    expect(layout.panes[1]!.members).toEqual([]);
    expect(within(paneB).queryByText("hello from bob")).not.toBeInTheDocument();
    expect(within(paneB).getByTestId("pane-empty")).toHaveTextContent(/add one .* pane header/i);
  });

  it("the pane actions menu adds an unassigned agent", async () => {
    await seedTwoAgentTranscripts();
    moveAgentToNewPane(PROJECT_ID, ROSTER_IDS, BOB.id);
    renderPanes();

    const paneB = paneEls()[1]!;
    await fireEvent.click(within(paneB).getByTestId("pane-member-remove"));
    await fireEvent.click(within(paneB).getByTestId("pane-actions"));
    await fireEvent.click(screen.getByTestId(`pane-add-agent-${BOB.id}`));

    expect(layoutFor(PROJECT_ID, ROSTER_IDS).panes[1]!.members).toEqual([BOB.id]);
    expect(within(paneEls()[1]!).getByText("hello from bob")).toBeInTheDocument();
  });

  it("the pane actions menu moves an agent from another pane", async () => {
    await seedTwoAgentTranscripts();
    moveAgentToNewPane(PROJECT_ID, ROSTER_IDS, BOB.id);
    renderPanes();

    const paneB = paneEls()[1]!;
    await fireEvent.click(within(paneB).getByTestId("pane-actions"));
    await fireEvent.click(screen.getByTestId(`pane-add-agent-${ALICE.id}`));

    const layout = layoutFor(PROJECT_ID, ROSTER_IDS);
    expect(layout.panes[0]!.members).toEqual([]);
    expect(layout.panes[1]!.members).toEqual([BOB.id, ALICE.id]);
    expect(within(paneEls()[0]!).queryByText("hello from alice")).not.toBeInTheDocument();
    expect(within(paneEls()[1]!).getByText("hello from alice")).toBeInTheDocument();
  });

  it("minimizes a pane without changing membership", async () => {
    await seedTwoAgentTranscripts();
    moveAgentToNewPane(PROJECT_ID, ROSTER_IDS, BOB.id);
    moveAgentToNewPane(PROJECT_ID, ROSTER_IDS, ALICE.id);
    renderPanes();

    await fireEvent.click(screen.getAllByTestId("pane-minimize")[1]!);

    expect(paneEls()).toHaveLength(2);
    expect(layoutFor(PROJECT_ID, ROSTER_IDS).minimized).toEqual([
      layoutFor(PROJECT_ID, ROSTER_IDS).panes[1]!.id,
    ]);
    expect(screen.queryByText("hello from bob")).not.toBeInTheDocument();
  });

  it("shows only maximize controls for exactly two panes", async () => {
    await seedTwoAgentTranscripts();
    moveAgentToNewPane(PROJECT_ID, ROSTER_IDS, BOB.id);
    renderPanes();

    expect(paneEls()).toHaveLength(2);
    expect(screen.getAllByTestId("pane-maximize")).toHaveLength(2);
    expect(screen.queryByTestId("pane-minimize")).not.toBeInTheDocument();
  });

  it("maximizes one pane and restores it from its header control", async () => {
    await seedTwoAgentTranscripts();
    moveAgentToNewPane(PROJECT_ID, ROSTER_IDS, BOB.id);
    setRecipients(PROJECT_ID, [ALICE.id]);
    renderPanes();

    await fireEvent.click(screen.getAllByTestId("pane-maximize")[1]!);

    expect(paneEls()).toHaveLength(1);
    expect(paneEls()[0]).toHaveAttribute("data-maximized", "true");
    expect(selectionFor(PROJECT_ID)).toEqual([BOB.id]);
    expect(screen.getByText("hello from bob")).toBeInTheDocument();
    expect(screen.queryByText("hello from alice")).not.toBeInTheDocument();

    await fireEvent.click(screen.getByTestId("pane-maximize"));
    expect(paneEls()).toHaveLength(2);
    expect(screen.getByText("hello from alice")).toBeInTheDocument();
    expect(screen.getByText("hello from bob")).toBeInTheDocument();
  });

  it("shows pane activity in headers", async () => {
    await seedTwoAgentTranscripts();
    const state = await loadState();
    const runtime = state.runtimes[BOB.id];
    if (runtime === undefined) throw new Error("runtime missing");
    state.runtimes[BOB.id] = {
      ...runtime,
      run_status: "starting",
      pending_sends: [{ send_id: "send-bob", user_turn_id: "user-bob" }],
    };
    moveAgentToNewPane(PROJECT_ID, ROSTER_IDS, BOB.id);
    renderPanes();

    expect(within(paneEls()[1]!).getByTestId("pane-activity")).toBeInTheDocument();
    expect(within(paneEls()[0]!).queryByTestId("pane-activity")).not.toBeInTheDocument();
  });
});

describe("visibility", () => {
  it("an all-hidden pane's Show all reveals only that pane", async () => {
    await seedTwoAgentTranscripts();
    moveAgentToNewPane(PROJECT_ID, ROSTER_IDS, BOB.id);
    toggleAgentHidden(PROJECT_ID, ROSTER_IDS, BOB.id);
    // alice deliberately hidden in pane 1 — pane 2's reset must not undo it.
    toggleAgentHidden(PROJECT_ID, ROSTER_IDS, ALICE.id);
    renderPanes();

    const paneB = paneEls()[1]!;
    expect(within(paneB).getByTestId("pane-empty")).toHaveTextContent(/all agents .* hidden/i);
    await fireEvent.click(within(paneB).getByTestId("pane-show-all"));

    expect(within(paneEls()[1]!).getByText("hello from bob")).toBeInTheDocument();
    expect(within(paneEls()[0]!).queryByText("hello from alice")).not.toBeInTheDocument();
    expect(within(paneEls()[0]!).getByTestId("pane-empty")).toBeInTheDocument();
  });

  it("an empty pane explains how to populate it", async () => {
    await seedTwoAgentTranscripts();
    const paneId = moveAgentToNewPane(PROJECT_ID, ROSTER_IDS, BOB.id);
    const pane1 = layoutFor(PROJECT_ID, ROSTER_IDS).panes[0]!.id;
    // Move bob back, emptying pane 2 (emptied panes stay open).
    const { moveAgentToPane } = await import("$lib/state/transcriptPanes.svelte");
    moveAgentToPane(PROJECT_ID, ROSTER_IDS, BOB.id, pane1);
    renderPanes();

    expect(layoutFor(PROJECT_ID, ROSTER_IDS).panes[1]!.id).toBe(paneId);
    expect(within(paneEls()[1]!).getByTestId("pane-empty")).toHaveTextContent(/move one here/i);
  });

  it("an empty pane is not a send target: no header affordance, Cmd+click is inert", async () => {
    await seedTwoAgentTranscripts();
    moveAgentToNewPane(PROJECT_ID, ROSTER_IDS, BOB.id);
    const pane1 = layoutFor(PROJECT_ID, ROSTER_IDS).panes[0]!.id;
    const { moveAgentToPane } = await import("$lib/state/transcriptPanes.svelte");
    moveAgentToPane(PROJECT_ID, ROSTER_IDS, BOB.id, pane1); // empties pane 2
    setRecipients(PROJECT_ID, [ALICE.id]);
    renderPanes();

    const paneB = paneEls()[1]!;
    // The name renders, but not as the "Send to" button.
    expect(within(paneB).getByTestId("pane-name")).toBeInTheDocument();
    expect(within(paneB).queryByTestId("pane-target")).not.toBeInTheDocument();

    // Cmd+click targeting an empty pane would only clear the recipient set.
    await fireEvent.click(paneB, { metaKey: true });
    expect(selectionFor(PROJECT_ID)).toEqual([ALICE.id]);
  });
});

describe("targeting", () => {
  it("header click replaces the recipient set with the pane's members", async () => {
    await seedTwoAgentTranscripts();
    moveAgentToNewPane(PROJECT_ID, ROSTER_IDS, BOB.id);
    setRecipients(PROJECT_ID, [ALICE.id]);
    renderPanes();

    await fireEvent.click(screen.getAllByTestId("pane-target")[1]!);
    expect(selectionFor(PROJECT_ID)).toEqual([BOB.id]);
  });

  it("Cmd+click anywhere in a pane targets it; plain click never does", async () => {
    await seedTwoAgentTranscripts();
    moveAgentToNewPane(PROJECT_ID, ROSTER_IDS, BOB.id);
    setRecipients(PROJECT_ID, [BOB.id]);
    renderPanes();

    // Plain click inside pane A (reading) leaves the draft target untouched.
    await fireEvent.click(paneEls()[0]!);
    expect(selectionFor(PROJECT_ID)).toEqual([BOB.id]);

    await fireEvent.click(paneEls()[0]!, { metaKey: true });
    expect(selectionFor(PROJECT_ID)).toEqual([ALICE.id]);
  });

  it("modified Cmd-click does not target a pane", async () => {
    await seedTwoAgentTranscripts();
    moveAgentToNewPane(PROJECT_ID, ROSTER_IDS, BOB.id);
    setRecipients(PROJECT_ID, [BOB.id]);
    renderPanes();

    await fireEvent.click(paneEls()[0]!, { metaKey: true, altKey: true });
    expect(selectionFor(PROJECT_ID)).toEqual([BOB.id]);

    await fireEvent.click(paneEls()[0]!, { metaKey: true, shiftKey: true });
    expect(selectionFor(PROJECT_ID)).toEqual([BOB.id]);
  });

  it("coverage border derives from the recipient set and cannot lie", async () => {
    await seedTwoAgentTranscripts();
    const paneId = moveAgentToNewPane(PROJECT_ID, ROSTER_IDS, BOB.id);
    const { moveAgentToPane } = await import("$lib/state/transcriptPanes.svelte");
    moveAgentToPane(PROJECT_ID, ROSTER_IDS, ALICE.id, paneId); // pane 2: alice+bob
    renderPanes();

    setRecipients(PROJECT_ID, [ALICE.id, BOB.id]);
    await Promise.resolve();
    expect(paneEls()[1]).toHaveAttribute("data-coverage", "full");
    // The indicator must be its own overlay ELEMENT: a ring on the pane
    // itself paints beneath the opaque header/transcript children and is
    // never visible (the regression a data-attribute assertion can't catch).
    expect(within(paneEls()[1]!).getByTestId("pane-coverage")).toBeInTheDocument();

    // Dropping one recipient instantly demotes the border to partial — the
    // core invariant: the visual cannot disagree with who actually receives.
    setRecipients(PROJECT_ID, [ALICE.id]);
    await Promise.resolve();
    expect(paneEls()[1]).toHaveAttribute("data-coverage", "partial");
    expect(within(paneEls()[1]!).getByTestId("pane-coverage")).toBeInTheDocument();

    setRecipients(PROJECT_ID, []);
    await Promise.resolve();
    expect(paneEls()[1]).toHaveAttribute("data-coverage", "none");
    expect(within(paneEls()[1]!).queryByTestId("pane-coverage")).not.toBeInTheDocument();
  });

  it("whole-roster selection shows every pane fully covered", async () => {
    await seedTwoAgentTranscripts();
    moveAgentToNewPane(PROJECT_ID, ROSTER_IDS, BOB.id);
    setRecipients(PROJECT_ID, [ALICE.id, BOB.id]);
    renderPanes();
    expect(paneEls()[0]).toHaveAttribute("data-coverage", "full");
    expect(paneEls()[1]).toHaveAttribute("data-coverage", "full");
  });
});

describe("Cmd-held target overlay", () => {
  it("arms on Cmd-down over the hovered pane, disarms on Cmd-up", async () => {
    await seedTwoAgentTranscripts();
    moveAgentToNewPane(PROJECT_ID, ROSTER_IDS, BOB.id);
    renderPanes();

    await fireEvent.pointerEnter(paneEls()[0]!);
    expect(screen.queryByTestId("pane-target-overlay")).not.toBeInTheDocument();

    await fireEvent.keyDown(window, { key: "Meta" });
    expect(screen.getByTestId("pane-target-overlay")).toBeInTheDocument();

    await fireEvent.keyUp(window, { key: "Meta" });
    expect(screen.queryByTestId("pane-target-overlay")).not.toBeInTheDocument();
  });

  it("does not arm when Cmd is combined with another modifier", async () => {
    await seedTwoAgentTranscripts();
    moveAgentToNewPane(PROJECT_ID, ROSTER_IDS, BOB.id);
    renderPanes();

    await fireEvent.pointerEnter(paneEls()[0]!);
    await fireEvent.keyDown(window, { key: "Meta", metaKey: true, altKey: true });
    expect(screen.queryByTestId("pane-target-overlay")).not.toBeInTheDocument();

    await fireEvent.keyDown(window, { key: "Alt", metaKey: true, altKey: true });
    expect(screen.queryByTestId("pane-target-overlay")).not.toBeInTheDocument();

    await fireEvent.keyUp(window, { key: "Alt", metaKey: true });
    expect(screen.getByTestId("pane-target-overlay")).toBeInTheDocument();

    await fireEvent.keyDown(window, { key: "Shift", metaKey: true, shiftKey: true });
    expect(screen.queryByTestId("pane-target-overlay")).not.toBeInTheDocument();
  });

  it("does not advertise Cmd-click targeting over an empty pane", async () => {
    await seedTwoAgentTranscripts();
    const emptyPaneId = moveAgentToNewPane(PROJECT_ID, ROSTER_IDS, BOB.id);
    const firstPaneId = layoutFor(PROJECT_ID, ROSTER_IDS).panes[0]!.id;
    moveAgentToPane(PROJECT_ID, ROSTER_IDS, BOB.id, firstPaneId);
    renderPanes();

    const emptyPane = paneEls().find((pane) => pane.dataset.paneId === emptyPaneId);
    if (emptyPane === undefined) throw new Error("expected empty pane");

    await fireEvent.pointerEnter(emptyPane);
    await fireEvent.keyDown(window, { key: "Meta" });
    expect(screen.queryByTestId("pane-target-overlay")).not.toBeInTheDocument();

    await fireEvent.pointerEnter(paneEls()[0]!);
    expect(screen.getByTestId("pane-target-overlay")).toBeInTheDocument();
  });

  it("follows the hovered pane", async () => {
    await seedTwoAgentTranscripts();
    moveAgentToNewPane(PROJECT_ID, ROSTER_IDS, BOB.id);
    renderPanes();

    await fireEvent.keyDown(window, { key: "Meta" });
    await fireEvent.pointerEnter(paneEls()[1]!);
    const overlay = screen.getByTestId("pane-target-overlay");
    expect(paneEls()[1]).toContainElement(overlay);

    await fireEvent.pointerLeave(paneEls()[1]!);
    expect(screen.queryByTestId("pane-target-overlay")).not.toBeInTheDocument();
  });

  it("disarms on window blur (Cmd+Tab away loses the keyup)", async () => {
    await seedTwoAgentTranscripts();
    moveAgentToNewPane(PROJECT_ID, ROSTER_IDS, BOB.id);
    renderPanes();

    await fireEvent.pointerEnter(paneEls()[0]!);
    await fireEvent.keyDown(window, { key: "Meta" });
    expect(screen.getByTestId("pane-target-overlay")).toBeInTheDocument();

    await fireEvent.blur(window);
    expect(screen.queryByTestId("pane-target-overlay")).not.toBeInTheDocument();
  });
});

describe("pane header shortcut hints", () => {
  beforeEach(() => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("only shows Cmd+Alt shortcut hints for panes 1 through 9", async () => {
    const agents = Array.from({ length: 10 }, (_, i) => numberedAgent(i + 1));
    const rosterIds = agents.map((agent) => agent.id);
    for (const agent of agents.slice(1)) {
      moveAgentToNewPane(PROJECT_ID, rosterIds, agent.id);
    }
    render(TranscriptPanes, { props: { projectId: PROJECT_ID, agents } });

    const targets = screen.getAllByTestId("pane-target");
    expect(targets).toHaveLength(10);

    await fireEvent.pointerEnter(targets[8]!);
    await vi.advanceTimersByTimeAsync(500);
    let content = await waitFor(() => screen.getByTestId("tooltip-content"));
    expect(content).toHaveTextContent("Send to Pane 9");
    expect(content).toHaveTextContent(shortcut("mod", "alt", "9"));

    await fireEvent.pointerLeave(targets[8]!);
    await fireEvent.pointerEnter(targets[9]!);
    await vi.advanceTimersByTimeAsync(500);
    content = await waitFor(() => screen.getByTestId("tooltip-content"));
    expect(content).toHaveTextContent("Send to Pane 10");
    expect(content).not.toHaveTextContent(shortcut("mod", "alt", "10"));
  });

  it("keeps shortcut hints aligned to full pane order when a pane is minimized", async () => {
    const carol = numberedAgent(3);
    const agents = [ALICE, BOB, carol];
    const rosterIds = agents.map((agent) => agent.id);
    const pane2 = moveAgentToNewPane(PROJECT_ID, rosterIds, BOB.id);
    moveAgentToNewPane(PROJECT_ID, rosterIds, carol.id);
    minimizePane(PROJECT_ID, rosterIds, pane2);
    render(TranscriptPanes, { props: { projectId: PROJECT_ID, agents } });

    const targets = screen.getAllByTestId("pane-target");
    expect(targets).toHaveLength(2);

    await fireEvent.pointerEnter(targets[1]!);
    await vi.advanceTimersByTimeAsync(500);
    const content = await waitFor(() => screen.getByTestId("tooltip-content"));
    expect(content).toHaveTextContent("Send to Pane 3");
    expect(content).toHaveTextContent(shortcut("mod", "alt", "3"));
    expect(content).not.toHaveTextContent(shortcut("mod", "alt", "2"));
  });
});
