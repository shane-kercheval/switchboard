import { afterEach, describe, expect, it, vi } from "vitest";
import "@testing-library/jest-dom/vitest";
import { tick } from "svelte";
import { fireEvent, render, screen } from "@testing-library/svelte";
import TranscriptNavigator from "./TranscriptNavigator.svelte";
import type { AgentRecord } from "$lib/types";

const invokeMock = vi.fn(async (_cmd: string, _args?: Record<string, unknown>) => null);
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: Record<string, unknown>) => invokeMock(cmd, args),
  convertFileSrc: (p: string) => `asset://localhost/${p}`,
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => vi.fn()) }));
vi.mock("$lib/native", () => ({ copyText: vi.fn(async () => undefined) }));

const state = await import("$lib/state/index.svelte");
const jump = await import("$lib/state/transcriptJump.svelte");
const panes = await import("$lib/state/transcriptPanes.svelte");

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
  ...ALICE,
  id: "00000000-0000-7000-8000-000000000bbb",
  name: "bob",
  session_locator: { uuid: "00000000-0000-7000-8000-000000000002" },
};

afterEach(() => {
  state._testing.reset();
  jump._testing.reset();
  panes._testing.reset();
});

async function seed(): Promise<void> {
  await state.registerAgent(ALICE);
  await state.registerAgent(BOB);
  state.transcripts[ALICE.id] = [
    {
      role: "user",
      turn_id: "u-1",
      agent_id: ALICE.id,
      started_at: "2026-05-16T00:00:00Z",
      text: "fix the login bug",
      send_id: "send-1",
    },
    {
      role: "agent",
      turn_id: "turn-a",
      agent_id: ALICE.id,
      started_at: "2026-05-16T00:00:01Z",
      status: "complete",
      items: [{ item_kind: "text", kind: "text", text: "deployed the fix" }],
    },
  ];
  state.transcripts[BOB.id] = [
    {
      role: "agent",
      turn_id: "turn-b",
      agent_id: BOB.id,
      started_at: "2026-05-16T00:00:02Z",
      status: "complete",
      items: [{ item_kind: "text", kind: "text", text: "reviewing the change" }],
    },
  ];
}

function props() {
  return { projectId: PROJECT_ID, agents: [ALICE, BOB] };
}

async function openNavigator(): Promise<void> {
  await fireEvent.click(screen.getByTestId("transcript-navigator-toggle"));
  await tick();
}

describe("TranscriptNavigator", () => {
  it("opens to a flat list of all messages, newest-first by default; the sort toggle flips it", async () => {
    await seed();
    render(TranscriptNavigator, { props: props() });

    expect(screen.queryByTestId("transcript-navigator")).toBeNull();
    await openNavigator();

    // Newest-first: bob (00:02) → alice (00:01) → You (00:00).
    const rowKeys = () =>
      screen.getAllByTestId("navigator-entry").map((e) => e.getAttribute("data-row-key"));
    expect(rowKeys()).toEqual(["a:turn-b", "a:turn-a", "u:send-1"]);

    await fireEvent.click(screen.getByTestId("navigator-sort"));
    expect(rowKeys()).toEqual(["u:send-1", "a:turn-a", "a:turn-b"]);
  });

  it("type-to-filter narrows the list; the role filter composes with it", async () => {
    await seed();
    render(TranscriptNavigator, { props: props() });
    await openNavigator();

    await fireEvent.input(screen.getByTestId("navigator-search"), {
      target: { value: "  FIX  " },
    });
    expect(screen.getAllByTestId("navigator-entry")).toHaveLength(2);

    await fireEvent.click(screen.getByText("Agents"));
    const entries = screen.getAllByTestId("navigator-entry");
    expect(entries).toHaveLength(1);
    expect(entries[0]).toHaveTextContent("alice");

    await fireEvent.input(screen.getByTestId("navigator-search"), {
      target: { value: "zzz" },
    });
    expect(screen.queryAllByTestId("navigator-entry")).toHaveLength(0);
    expect(screen.getByTestId("navigator-empty")).toHaveTextContent("No matches.");
  });

  it("↑/↓ move the highlight with the preview following; ↵ jumps and closes", async () => {
    await seed();
    render(TranscriptNavigator, { props: props() });
    await openNavigator();

    const search = screen.getByTestId("navigator-search");
    await fireEvent.keyDown(search, { key: "ArrowDown" });
    await tick();
    expect(screen.getAllByTestId("navigator-entry")[1]).toHaveAttribute("aria-selected", "true");
    // Keyboard movement shows the preview immediately (no hover debounce).
    expect(screen.getByTestId("navigator-preview")).toHaveTextContent("deployed the fix");

    await fireEvent.keyDown(search, { key: "Enter" });
    // Jump addressed to alice's pane (the unified default) with her turn's row key.
    expect(jump.jumpRequest.rowKey).toBe("a:turn-a");
    expect(jump.jumpRequest.projectId).toBe(PROJECT_ID);
    expect(jump.jumpRequest.paneId).not.toBeNull();
    expect(screen.queryByTestId("transcript-navigator")).toBeNull(); // closed
  });

  it("renders the complete selected message in the scrollable preview", async () => {
    await seed();
    const turn = state.transcripts[ALICE.id]?.[1];
    if (turn?.role !== "agent") throw new Error("expected seeded agent turn");
    turn.items = [
      {
        item_kind: "text",
        kind: "text",
        text: `start ${"x".repeat(1_600)} complete-message-tail`,
      },
    ];
    render(TranscriptNavigator, { props: props() });
    await openNavigator();

    await fireEvent.keyDown(screen.getByTestId("navigator-search"), { key: "ArrowDown" });
    await tick();

    const preview = screen.getByTestId("navigator-preview");
    expect(preview).toHaveClass("overflow-y-auto");
    expect(preview).toHaveTextContent("complete-message-tail");
    expect(screen.queryByTestId("navigator-preview-truncated")).toBeNull();
  });

  it("clicking an entry jumps and closes the overlay", async () => {
    await seed();
    render(TranscriptNavigator, { props: props() });
    await openNavigator();

    const userEntry = screen
      .getAllByTestId("navigator-entry")
      .find((e) => e.getAttribute("data-row-key") === "u:send-1");
    await fireEvent.click(userEntry!);

    expect(jump.jumpRequest.rowKey).toBe("u:send-1");
    // The overlay is bound to the shared store; jumping closes it.
    expect(jump.navigatorState.open).toBe(false);
    await tick();
    expect(screen.queryByTestId("transcript-navigator")).toBeNull();
  });

  it("disables entries whose agent is hidden in its pane; clicking one is a no-op", async () => {
    await seed();
    panes.toggleAgentHidden(PROJECT_ID, [ALICE.id, BOB.id], BOB.id);
    render(TranscriptNavigator, { props: props() });
    await openNavigator();

    const bobEntry = screen
      .getAllByTestId("navigator-entry")
      .find((e) => e.getAttribute("data-row-key") === "a:turn-b");
    expect(bobEntry).toHaveAttribute("aria-disabled", "true");

    await fireEvent.click(bobEntry!);
    expect(jump.jumpRequest.rowKey).toBeNull(); // disabled → no jump
    expect(jump.navigatorState.open).toBe(true); // stays open
  });

  it("shows the empty state on a project with no messages", async () => {
    await state.registerAgent(ALICE);
    render(TranscriptNavigator, { props: { projectId: PROJECT_ID, agents: [ALICE] } });
    await openNavigator();
    expect(screen.getByTestId("navigator-empty")).toHaveTextContent("No messages yet.");
  });
});
