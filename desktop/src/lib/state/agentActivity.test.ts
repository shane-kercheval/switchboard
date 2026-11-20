import { describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => vi.fn()) }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));

import {
  activityPhrase,
  deriveAgentActivity,
  type ActivityInputs,
  type AgentActivity,
} from "./agentActivity.svelte";
import type { AgentRuntime } from "./types";
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

  it("reads working with the running turn's start", () => {
    const activity = derive(IMPL, {
      runtimes: {
        [IMPL.id]: runtime(IMPL.id, {
          run_status: "processing",
          turn_started_at: "2026-05-16T00:00:10Z",
        }),
      },
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

    it("names only the sources still running, while any is", () => {
      const activity = derive(DOCS, {
        runtimes: {
          [IMPL.id]: runtime(IMPL.id),
          [REVIEW.id]: runtime(REVIEW.id, { run_status: "processing" }),
        },
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
        on: [{ kind: "agent", agentId: REVIEW.id, name: "reviewer", harness: "codex" }],
      });
    });

    it("keeps a source whose state this window doesn't hold, and drops an idle one", () => {
      const unopened = { id: "00000000-0000-7000-8000-000000000009", name: "planner" };
      const activity = derive(DOCS, {
        runtimes: { [IMPL.id]: runtime(IMPL.id) },
        heldForwards: [held([{ id: IMPL.id, name: IMPL.name }, unopened], [DOCS.id])],
      });
      expect(activity.kind === "waiting" && activity.on.map((t) => t.name)).toEqual(["planner"]);
    });

    it("names every source when all have gone idle but the forward is still held", () => {
      const activity = derive(DOCS, {
        heldForwards: [held([{ id: IMPL.id, name: IMPL.name }], [DOCS.id])],
      });
      expect(activity.kind === "waiting" && activity.on.map((t) => t.name)).toEqual([
        "implementer",
      ]);
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

    it("waits only on the agents of the current step, not every working agent of the run", () => {
      const fanOut = [
        step("send", ["implementer", "reviewer"]),
        step("wait", ["reviewer"]),
        step("send", ["docs-writer"]),
      ];
      const bothWorking = {
        [IMPL.id]: runtime(IMPL.id, { run_status: "processing" }),
        [REVIEW.id]: runtime(REVIEW.id, { run_status: "processing" }),
        [DOCS.id]: runtime(DOCS.id),
      };
      expect(derive(DOCS, { runs: [run(1, fanOut)], runtimes: bothWorking })).toEqual({
        kind: "waiting",
        on: [{ kind: "agent", agentId: REVIEW.id, name: "reviewer", harness: "codex" }],
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
        on: [
          { kind: "agent", agentId: IMPL.id, name: "implementer" },
          { kind: "workflow", name: "review-loop" },
        ],
      }),
    ).toBe("waiting for implementer, review-loop");
    expect(activityPhrase({ kind: "failed", reason: "boom" })).toBe("last turn failed");
    expect(activityPhrase({ kind: "idle" })).toBeNull();
  });
});
