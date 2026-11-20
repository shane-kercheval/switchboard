import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/svelte";
import { tick } from "svelte";
import type {
  AgentRecord,
  NormalizedEvent,
  WorkflowProgressPayload,
  WorkflowRunInfo,
} from "$lib/types";
// Static import so the component-tree transform happens at module collection,
// not inside the first test's timeout. `vi.mock` is hoisted above imports.
import Sidebar from "./Sidebar.svelte";

/// The agent card's status line and ring, end to end through the real stores:
/// events arrive on the captured per-agent and per-project channels, and only
/// the backend calls are faked. `agentActivity.test.ts` covers the derivation's
/// rules; this file covers what the card shows as events arrive.

const listeners = new Map<string, (e: { payload: unknown }) => void>();
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (name: string, cb: (e: { payload: unknown }) => void) => {
    listeners.set(name, cb);
    return vi.fn();
  }),
}));

let listedRuns: WorkflowRunInfo[] = [];
const invokeMock = vi.fn(async (cmd: string): Promise<unknown> => {
  if (cmd === "list_workflow_runs") return listedRuns;
  return null;
});
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string) => invokeMock(cmd),
}));

const PROJECT_ID = "00000000-0000-7000-8000-0000000000ff";

function agent(n: number, name: string, harness: AgentRecord["harness"]): AgentRecord {
  return {
    id: `00000000-0000-7000-8000-00000000000${n}`,
    project_id: PROJECT_ID,
    name,
    harness,
    session_locator: null,
    model: "opus",
    effort: "high",
    model_choices: ["opus", "sonnet"],
    effort_choices: ["high", "medium"],
    created_at: "2026-05-16T00:00:00Z",
  };
}

const IMPL = agent(1, "implementer", "claude_code");
const REVIEW = agent(2, "reviewer", "codex");
const DOCS = agent(3, "docs-writer", "claude_code");
const ROSTER = [IMPL, REVIEW, DOCS];

function fireTo(channel: string, payload: NormalizedEvent | WorkflowProgressPayload): void {
  const cb = listeners.get(channel);
  if (cb === undefined) throw new Error(`no listener for ${channel}`);
  cb({ payload });
}

async function agentState() {
  return await import("$lib/state/index.svelte");
}

async function openProject(): Promise<void> {
  const ws = await import("$lib/state/workspace.svelte");
  const state = await agentState();
  ws.agentsByProject[PROJECT_ID] = ROSTER;
  for (const a of ROSTER) await state.registerAgent(a);
  render(Sidebar, { props: { projectId: PROJECT_ID, agents: ROSTER } });
}

function card(target: AgentRecord): HTMLElement {
  const found = screen
    .getAllByTestId("sidebar-agent")
    .find((el) => el.getAttribute("aria-label")?.startsWith(`${target.name},`));
  if (found === undefined) throw new Error(`no card for ${target.name}`);
  return found;
}

async function hoverTooltip(trigger: HTMLElement): Promise<string> {
  vi.useFakeTimers({ shouldAdvanceTime: true });
  try {
    await fireEvent.pointerEnter(trigger);
    await vi.advanceTimersByTimeAsync(1100);
    return (screen.getByTestId("tooltip-content").textContent ?? "").trim();
  } finally {
    vi.useRealTimers();
  }
}

/// A send to `target` the way the compose bar makes one: the pending row, then
/// the accepted receipt.
async function send(target: AgentRecord, n: number, withReceipt = true): Promise<void> {
  const state = await agentState();
  state.dispatchUserTurn(target.id, `user-${n}`, "go", [], `send-${n}`, "2026-05-16T00:00:00Z");
  if (withReceipt) state.recordSendAccepted(target.id, `user-${n}`, `msg-${n}`);
}

function turnStart(n: number, startedAt = "2026-05-16T00:00:01Z"): NormalizedEvent {
  return {
    type: "turn_start",
    turn_id: `turn-${n}`,
    message_id: `msg-${n}`,
    send_id: `send-${n}`,
    started_at: startedAt,
  };
}

beforeEach(async () => {
  listeners.clear();
  listedRuns = [];
  invokeMock.mockClear();
  (await import("$lib/state/transcriptPanes.svelte"))._testing.reset();
  (await import("$lib/state/heldForwards.svelte"))._testing.reset();
  (await import("$lib/state/workflows.svelte"))._testing.reset();
});

afterEach(async () => {
  vi.useRealTimers();
  (await agentState())._testing.reset();
  (await import("$lib/state/workspace.svelte"))._testing.reset();
});

describe("an agent card's status", () => {
  it("shows the model and effort while idle, with no ring", async () => {
    await openProject();
    const idle = card(DOCS);
    expect(idle).toHaveAttribute("data-activity", "idle");
    expect(idle.getAttribute("aria-label")).toBe(
      "docs-writer, collapsed. Press Enter or Space to toggle details.",
    );
    expect(within(idle).queryByTestId("activity-ring")).toBeNull();
    expect(within(idle).queryByTestId("agent-status")).toBeNull();
    expect(within(idle).getByTestId("agent-selection")).not.toHaveAttribute("inert");
  });

  it("works from the send, through the turn, to a failure, and clears it on the next send", async () => {
    await openProject();
    await send(DOCS, 1);
    await tick();
    expect(card(DOCS)).toHaveAttribute("data-activity", "working");
    expect(within(card(DOCS)).getByTestId("activity-ring")).toBeInTheDocument();
    // Queued, no turn yet: "Working" with no elapsed time.
    expect(within(card(DOCS)).queryByTestId("agent-status-elapsed")).toBeNull();

    fireTo(`agent:${DOCS.id}`, turnStart(1));
    await tick();
    expect(within(card(DOCS)).getByTestId("agent-status-elapsed")).toBeInTheDocument();

    fireTo(`agent:${DOCS.id}`, {
      type: "turn_end",
      turn_id: "turn-1",
      outcome: { status: "failed", kind: "harness_error", message: "claude exited with code 1" },
      ended_at: "2026-05-16T00:00:05Z",
    });
    fireTo(`agent:${DOCS.id}`, { type: "agent_idle", agent_id: DOCS.id });
    await tick();
    const failed = card(DOCS);
    expect(failed).toHaveAttribute("data-activity", "failed");
    expect(failed.getAttribute("aria-label")).toContain("docs-writer, last turn failed,");
    expect(within(failed).queryByTestId("activity-ring")).toBeNull();
    const status = within(failed).getByTestId("agent-status");
    expect(status).toHaveTextContent("Last turn failed");
    expect(await hoverTooltip(status)).toContain("claude exited with code 1");

    await send(DOCS, 2);
    await tick();
    expect(card(DOCS)).toHaveAttribute("data-activity", "working");
    fireTo(`agent:${DOCS.id}`, turnStart(2));
    fireTo(`agent:${DOCS.id}`, {
      type: "turn_end",
      turn_id: "turn-2",
      outcome: { status: "completed" },
      ended_at: "2026-05-16T00:00:09Z",
    });
    fireTo(`agent:${DOCS.id}`, { type: "agent_idle", agent_id: DOCS.id });
    await tick();
    expect(card(DOCS)).toHaveAttribute("data-activity", "idle");
  });

  it("shows a send that failed before it started as failed", async () => {
    await openProject();
    await send(DOCS, 1);
    fireTo(`agent:${DOCS.id}`, {
      type: "message_failed",
      message_id: "msg-1",
      send_id: "send-1",
      agent_id: DOCS.id,
      error: "adapter failed to launch",
      at: "2026-05-16T00:00:01Z",
    });
    await tick();
    expect(card(DOCS)).toHaveAttribute("data-activity", "failed");
  });

  it("does not show a turn the user cancelled as failed", async () => {
    await openProject();
    await send(DOCS, 1);
    fireTo(`agent:${DOCS.id}`, turnStart(1));
    fireTo(`agent:${DOCS.id}`, {
      type: "turn_end",
      turn_id: "turn-1",
      outcome: { status: "cancelled", source: "user" },
      ended_at: "2026-05-16T00:00:05Z",
    });
    fireTo(`agent:${DOCS.id}`, { type: "agent_idle", agent_id: DOCS.id });
    await tick();
    expect(card(DOCS)).toHaveAttribute("data-activity", "idle");
  });

  it("shows working when the turn starts before the send's receipt arrives", async () => {
    await openProject();
    await send(DOCS, 1, false);
    fireTo(`agent:${DOCS.id}`, turnStart(1));
    await tick();
    expect(card(DOCS)).toHaveAttribute("data-activity", "working");
    expect(within(card(DOCS)).getByTestId("agent-status-elapsed")).toBeInTheDocument();
    (await agentState()).recordSendAccepted(DOCS.id, "user-1", "msg-1");
    await tick();
    expect(card(DOCS)).toHaveAttribute("data-activity", "working");
  });

  it("counts the running turn's time from its start, once a second", async () => {
    vi.useFakeTimers({ toFake: ["Date", "setInterval", "clearInterval"] });
    vi.setSystemTime(new Date("2026-05-16T00:01:15Z"));
    await openProject();
    await send(DOCS, 1);
    fireTo(`agent:${DOCS.id}`, turnStart(1));
    await tick();
    const elapsed = within(card(DOCS)).getByTestId("agent-status-elapsed");
    expect(elapsed).toHaveTextContent("1m 14s");
    vi.advanceTimersByTime(1000);
    await tick();
    expect(elapsed).toHaveTextContent("1m 15s");
  });

  it("covers the model chips while working, out of the tab order, and names them on hover", async () => {
    await openProject();
    await send(DOCS, 1);
    await tick();
    const working = card(DOCS);
    const chips = within(working).getByTestId("agent-selection");
    // jsdom has no `inert`, so Svelte's property write leaves no attribute here;
    // the browser suite checks the real attribute.
    expect((chips as HTMLElement & { inert?: boolean }).inert).toBe(true);
    expect(chips).toHaveClass("invisible");
    const status = within(working).getByTestId("agent-status");
    expect(status).toHaveTextContent("Working");
    expect(await hoverTooltip(status)).toBe("Model: Opus · Effort: High");
    // The accessible name carries the state, not the ticking time.
    expect(working.getAttribute("aria-label")).toBe(
      "docs-writer, working, collapsed. Press Enter or Space to toggle details.",
    );
  });

  it("waits on a held forward's sources until the forward is released", async () => {
    await openProject();
    const held = await import("$lib/state/heldForwards.svelte");
    held.addHeldForward(PROJECT_ID, {
      forwardId: "f1",
      sendId: "send-f",
      body: "",
      sources: [
        { id: IMPL.id, name: IMPL.name },
        { id: REVIEW.id, name: REVIEW.name },
      ],
      recipients: [DOCS.id],
    });
    await tick();
    const waiting = card(DOCS);
    expect(waiting).toHaveAttribute("data-activity", "waiting");
    expect(within(waiting).queryByTestId("activity-ring")).toBeNull();
    const status = within(waiting).getByTestId("agent-status");
    expect(within(status).getByTestId("agent-status-wait-chip")).toHaveTextContent("implementer");
    expect(within(status).getByTestId("agent-status-wait-more")).toHaveTextContent("+1");
    expect(waiting.getAttribute("aria-label")).toContain("waiting for implementer, reviewer");
    expect(await hoverTooltip(status)).toBe("Waiting for implementer, reviewer");

    held.removeHeldForward(PROJECT_ID, "f1");
    await tick();
    expect(card(DOCS)).toHaveAttribute("data-activity", "idle");
  });

  it("waits on a running workflow's working agent until its own step comes", async () => {
    listedRuns = [
      {
        run_id: "run-1",
        workflow: "review-loop",
        step: 0,
        total: 2,
        status: "running",
        reason: null,
        steps: [
          {
            kind: "send",
            label: "implement",
            description: null,
            prompt: null,
            recipients: [{ kind: "literal", name: "implementer" }],
            feeds_from: [],
          },
          {
            kind: "send",
            label: "review",
            description: null,
            prompt: null,
            recipients: [{ kind: "literal", name: "reviewer" }],
            feeds_from: [],
          },
        ],
      },
    ];
    await openProject();
    const workflows = await import("$lib/state/workflows.svelte");
    await workflows.subscribeProjectWorkflows(PROJECT_ID);
    await send(IMPL, 1);
    fireTo(`agent:${IMPL.id}`, turnStart(1));
    await tick();
    expect(card(REVIEW)).toHaveAttribute("data-activity", "waiting");
    expect(within(card(REVIEW)).getByTestId("agent-status-wait-chip")).toHaveTextContent(
      "implementer",
    );

    fireTo(`workflow:${PROJECT_ID}`, {
      run_id: "run-1",
      workflow: "review-loop",
      step: 1,
      total: 2,
      status: "running",
      reason: null,
    });
    await tick();
    expect(card(REVIEW)).toHaveAttribute("data-activity", "idle");
  });

  it("names the workflow when none of its agents is working", async () => {
    listedRuns = [
      {
        run_id: "run-1",
        workflow: "review-loop",
        step: 0,
        total: 2,
        status: "running",
        reason: null,
        steps: [
          {
            kind: "wait",
            label: "wait",
            description: null,
            prompt: null,
            recipients: [{ kind: "literal", name: "implementer" }],
            feeds_from: [],
          },
          {
            kind: "send",
            label: "review",
            description: null,
            prompt: null,
            recipients: [{ kind: "literal", name: "reviewer" }],
            feeds_from: [],
          },
        ],
      },
    ];
    await openProject();
    await (await import("$lib/state/workflows.svelte")).subscribeProjectWorkflows(PROJECT_ID);
    await waitFor(() => expect(card(REVIEW)).toHaveAttribute("data-activity", "waiting"));
    expect(within(card(REVIEW)).getByTestId("agent-status")).toHaveTextContent(
      "Waiting for review-loop",
    );
  });
});
