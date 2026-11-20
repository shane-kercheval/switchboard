import { describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => vi.fn()) }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));

import {
  activityPhrase,
  deriveAgentActivity,
  type ActivityInputs,
  type AgentActivity,
} from "./agentActivity.svelte";
import type { AgentRuntime, Turn } from "./types";
import type { HeldForward } from "./heldForwards.svelte";
import type { AgentRecord, WorkflowRunInfo, WorkflowStepInfo } from "$lib/types";

const PROJECT = "00000000-0000-7000-8000-0000000000ff";
const OTHER_PROJECT = "00000000-0000-7000-8000-0000000000ee";

function agent(n: number, name: string, harness: AgentRecord["harness"] = "claude_code") {
  return {
    id: `00000000-0000-7000-8000-00000000000${n}`,
    project_id: PROJECT,
    name,
    harness,
    session_locator: null,
    model: null,
    effort: null,
    model_choices: [],
    effort_choices: [],
    created_at: "2026-05-16T00:00:00Z",
  } satisfies AgentRecord;
}

const IMPL = agent(1, "implementer");
const REVIEW = agent(2, "reviewer", "codex");
const DOCS = agent(3, "docs-writer");
const ROSTER = [IMPL, REVIEW, DOCS];

function runtime(id: string, patch: Partial<AgentRuntime> = {}): AgentRuntime {
  return { agent_id: id, run_status: "idle", hydration_status: "complete", ...patch };
}

function streamingTurn(id: string, agentId: string, startedAt: string): Turn {
  return {
    role: "agent",
    turn_id: id,
    agent_id: agentId,
    started_at: startedAt,
    status: "streaming",
    items: [],
  };
}

function step(kind: WorkflowStepInfo["kind"], names: string[]): WorkflowStepInfo {
  return {
    kind,
    label: kind,
    description: null,
    prompt: null,
    recipients: names.map((name) => ({ kind: "literal", name })),
    feeds_from: [],
  };
}

function run(stepIndex: number, steps: WorkflowStepInfo[], patch: Partial<WorkflowRunInfo> = {}) {
  return {
    run_id: "run-1",
    workflow: "review-loop",
    step: stepIndex,
    total: steps.length,
    status: "running",
    reason: null,
    steps,
    ...patch,
  } satisfies WorkflowRunInfo;
}

function held(sources: { id: string; name: string; projectId?: string }[], recipients: string[]) {
  return {
    forwardId: `f-${recipients.join("-")}`,
    sendId: "send-f",
    body: "",
    sources,
    recipients,
  } satisfies HeldForward;
}

function derive(target: AgentRecord, patch: Partial<ActivityInputs> = {}): AgentActivity {
  return deriveAgentActivity({
    agent: target,
    projectId: PROJECT,
    roster: ROSTER,
    runtimes: Object.fromEntries(ROSTER.map((a) => [a.id, runtime(a.id)])),
    transcript: [],
    heldForwards: [],
    runs: [],
    findAgent: () => undefined,
    ...patch,
  });
}

describe("deriveAgentActivity", () => {
  it("reads idle when nothing applies", () => {
    expect(derive(DOCS)).toEqual({ kind: "idle" });
  });

  it("reads working with the streaming turn's start while a turn runs", () => {
    const activity = derive(IMPL, {
      runtimes: {
        [IMPL.id]: runtime(IMPL.id, { run_status: "processing", in_flight_turn_id: "t1" }),
      },
      transcript: [streamingTurn("t1", IMPL.id, "2026-05-16T00:00:10Z")],
    });
    expect(activity).toEqual({ kind: "working", startedAt: "2026-05-16T00:00:10Z" });
  });

  it("counts a send that is only queued as working, with no elapsed time yet", () => {
    const activity = derive(IMPL, {
      runtimes: {
        [IMPL.id]: runtime(IMPL.id, {
          run_status: "starting",
          pending_sends: [{ send_id: "s1", user_turn_id: "u1" }],
        }),
      },
    });
    expect(activity).toEqual({ kind: "working", startedAt: undefined });
  });

  it("does not count a send the user is cancelling", () => {
    const activity = derive(IMPL, {
      runtimes: {
        [IMPL.id]: runtime(IMPL.id, {
          pending_sends: [{ send_id: "s1", user_turn_id: "u1", cancel_requested: true }],
        }),
      },
    });
    expect(activity).toEqual({ kind: "idle" });
  });

  it("reads failed with the reason, and working outranks it", () => {
    const failedRuntime = runtime(DOCS.id, {
      last_error: { message: "The CLI exited with status 1.", kind: "harness_error" },
    });
    expect(derive(DOCS, { runtimes: { [DOCS.id]: failedRuntime } })).toEqual({
      kind: "failed",
      reason: "The CLI exited with status 1.",
    });
    const busyAgain = { ...failedRuntime, run_status: "processing" as const };
    expect(derive(DOCS, { runtimes: { [DOCS.id]: busyAgain } }).kind).toBe("working");
  });

  describe("waiting on a held forward", () => {
    it("names each source, with its harness from the roster", () => {
      const activity = derive(DOCS, {
        heldForwards: [
          held(
            [
              { id: IMPL.id, name: IMPL.name },
              { id: REVIEW.id, name: REVIEW.name },
            ],
            [DOCS.id],
          ),
        ],
      });
      expect(activity).toEqual({
        kind: "waiting",
        on: [
          { kind: "agent", agentId: IMPL.id, name: "implementer", harness: "claude_code" },
          { kind: "agent", agentId: REVIEW.id, name: "reviewer", harness: "codex" },
        ],
      });
    });

    it("labels a source in another project with that project, and looks up its harness", () => {
      const foreign = { ...agent(9, "planner", "codex"), project_id: OTHER_PROJECT };
      const activity = derive(DOCS, {
        heldForwards: [
          held([{ id: foreign.id, name: "planner", projectId: OTHER_PROJECT }], [DOCS.id]),
        ].map((f) => ({
          ...f,
          sources: f.sources.map((s) => ({ ...s, projectName: "planning" })),
        })),
        findAgent: (id) => (id === foreign.id ? foreign : undefined),
      });
      expect(activity).toEqual({
        kind: "waiting",
        on: [{ kind: "agent", agentId: foreign.id, name: "planning · planner", harness: "codex" }],
      });
    });

    it("ignores a forward held for another agent", () => {
      const activity = derive(DOCS, {
        heldForwards: [held([{ id: IMPL.id, name: IMPL.name }], [REVIEW.id])],
      });
      expect(activity.kind).toBe("idle");
    });

    it("outranks a failure, and is outranked by working", () => {
      const forwards = [held([{ id: IMPL.id, name: IMPL.name }], [DOCS.id])];
      const failed = runtime(DOCS.id, { last_error: { message: "x", kind: "harness_error" } });
      expect(derive(DOCS, { heldForwards: forwards, runtimes: { [DOCS.id]: failed } }).kind).toBe(
        "waiting",
      );
      const busy = runtime(DOCS.id, { run_status: "processing" });
      expect(derive(DOCS, { heldForwards: forwards, runtimes: { [DOCS.id]: busy } }).kind).toBe(
        "working",
      );
    });
  });

  describe("waiting in a workflow", () => {
    const steps = [
      step("send", ["implementer"]),
      step("wait", ["implementer"]),
      step("send", ["reviewer"]),
      step("wait", ["reviewer"]),
    ];
    const implWorking = {
      [IMPL.id]: runtime(IMPL.id, { run_status: "processing" }),
      [REVIEW.id]: runtime(REVIEW.id),
    };

    it("waits on the run's working agents when a later step names it", () => {
      expect(derive(REVIEW, { runs: [run(1, steps)], runtimes: implWorking })).toEqual({
        kind: "waiting",
        on: [{ kind: "agent", agentId: IMPL.id, name: "implementer", harness: "claude_code" }],
      });
    });

    it("matches step names the way the backend compares names", () => {
      const renamed = [step("send", ["implementer"]), step("send", ["REVIEWER"])];
      expect(derive(REVIEW, { runs: [run(0, renamed)], runtimes: implWorking }).kind).toBe(
        "waiting",
      );
    });

    it("is not waiting once its own step is current or past", () => {
      expect(derive(REVIEW, { runs: [run(2, steps)], runtimes: implWorking }).kind).toBe("idle");
    });

    it("is not waiting for a run that is not running", () => {
      const failedRun = run(1, steps, { status: "failed" });
      expect(derive(REVIEW, { runs: [failedRun], runtimes: implWorking }).kind).toBe("idle");
    });

    it("waits on the user when the run is paused for input", () => {
      const paused = [step("send", ["implementer"]), step("pause", []), step("send", ["reviewer"])];
      expect(derive(REVIEW, { runs: [run(1, paused)] })).toEqual({
        kind: "waiting",
        on: [{ kind: "user" }],
      });
    });

    it("waits on the run by name when none of its agents is working", () => {
      expect(derive(REVIEW, { runs: [run(1, steps)] })).toEqual({
        kind: "waiting",
        on: [{ kind: "workflow", name: "review-loop" }],
      });
    });

    it("ignores a run whose steps have not been fetched yet", () => {
      expect(derive(REVIEW, { runs: [run(1, [])], runtimes: implWorking }).kind).toBe("idle");
    });

    it("lists each agent once across a held forward and a workflow", () => {
      const activity = derive(REVIEW, {
        runs: [run(1, steps)],
        runtimes: implWorking,
        heldForwards: [held([{ id: IMPL.id, name: IMPL.name }], [REVIEW.id])],
      });
      expect(activity.kind === "waiting" && activity.on).toHaveLength(1);
    });
  });
});

describe("activityPhrase", () => {
  it("names each state without an elapsed time", () => {
    expect(activityPhrase({ kind: "working", startedAt: "2026-05-16T00:00:00Z" })).toBe("working");
    expect(
      activityPhrase({
        kind: "waiting",
        on: [{ kind: "agent", agentId: IMPL.id, name: "implementer" }, { kind: "user" }],
      }),
    ).toBe("waiting for implementer, you");
    expect(activityPhrase({ kind: "failed", reason: "boom" })).toBe("last turn failed");
    expect(activityPhrase({ kind: "idle" })).toBeNull();
  });
});
