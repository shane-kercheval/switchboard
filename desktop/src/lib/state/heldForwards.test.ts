import { describe, expect, it } from "vitest";
import {
  agentReadinessFor,
  expandForwardSources,
  forwardReadiness,
  forwardSourceAgentsForPane,
  forwardSourceLabel,
  orderForwardSources,
  reconcileForwardSourceMap,
  reconcileForwardSources,
} from "./heldForwards.svelte";
import type { ForwardSource } from "./heldForwards.svelte";
import type { Turn, TurnItem } from "./types";
import type { AgentRecord } from "$lib/types";
import type { TranscriptPane } from "$lib/state/transcriptPanes.svelte";

const agent = (id: string, name: string): AgentRecord => ({
  id,
  project_id: "00000000-0000-7000-8000-0000000000ff",
  name,
  harness: "claude_code",
  session_locator: null,
  model: null,
  effort: null,
  model_choices: [],
  effort_choices: [],
  created_at: "2026-05-16T00:00:00Z",
});

const ALICE = agent("agent-a", "alice");
const BOB = agent("agent-b", "bob");
const ROSTER = [ALICE, BOB];

const PROJECT = "00000000-0000-7000-8000-0000000000ff";
const source = (id: string, name: string): ForwardSource => ({ id, name, projectId: PROJECT });

describe("cross-project forward sources", () => {
  const OTHER = "00000000-0000-7000-8000-0000000000aa";

  it("keeps a foreign source on restore instead of dropping it", () => {
    // The current project's roster never contains a foreign agent, so matching
    // on it would delete every cross-project chip on each remount.
    const foreign: ForwardSource = {
      id: "agent-z",
      name: "oracle",
      projectId: OTHER,
      projectName: "backend",
    };
    expect(reconcileForwardSources([foreign], ROSTER, PROJECT)).toEqual([foreign]);
  });

  it("still drops a local source whose agent left the roster", () => {
    const gone = source("agent-gone", "ghost");
    expect(reconcileForwardSources([gone], ROSTER, PROJECT)).toEqual([]);
  });

  it("upgrades a legacy source with no project to the draft's own project", () => {
    // Drafts persisted before sources carried an owner: the wire type requires
    // one, so the ambiguity must be resolved here rather than travelling.
    const legacy = { id: "agent-a", name: "alice" } as ForwardSource;
    expect(reconcileForwardSources([legacy], ROSTER, PROJECT)).toEqual([
      { id: "agent-a", name: "alice", projectId: PROJECT },
    ]);
  });

  it("sends each source's own project on the wire", () => {
    const local = source("agent-a", "alice");
    const foreign: ForwardSource = { id: "agent-z", name: "oracle", projectId: OTHER };
    expect(expandForwardSources([local, foreign], PROJECT)).toEqual([
      { agent_id: "agent-a", project_id: PROJECT },
      { agent_id: "agent-z", project_id: OTHER },
    ]);
  });

  it("defaults a source with no project to the composing project", () => {
    const legacy = { id: "agent-a", name: "alice" } as ForwardSource;
    expect(expandForwardSources([legacy], PROJECT)).toEqual([
      { agent_id: "agent-a", project_id: PROJECT },
    ]);
  });

  it("qualifies only foreign labels with the project name", () => {
    const local = source("agent-a", "alice");
    const foreign: ForwardSource = {
      id: "agent-z",
      name: "alice",
      projectId: OTHER,
      projectName: "backend",
    };
    expect(forwardSourceLabel(local, PROJECT)).toBe("alice");
    // Same agent name in two projects must stay distinguishable.
    expect(forwardSourceLabel(foreign, PROJECT)).toBe("backend · alice");
  });

  it("falls back to the bare name when a foreign project's name is unknown", () => {
    const foreign: ForwardSource = { id: "agent-z", name: "oracle", projectId: OTHER };
    expect(forwardSourceLabel(foreign, PROJECT)).toBe("oracle");
  });
});

describe("orderForwardSources", () => {
  it("matches the agent-card roster order regardless of selection order", () => {
    expect(
      orderForwardSources(
        [source("agent-b", "stale-bob"), source("agent-a", "stale-alice")],
        ROSTER,
        PROJECT,
      ),
    ).toEqual([
      { id: "agent-a", name: "alice", projectId: PROJECT },
      { id: "agent-b", name: "bob", projectId: PROJECT },
    ]);
  });

  it("omits a local source whose agent left the roster", () => {
    expect(
      orderForwardSources(
        [source("agent-b", "bob"), source("agent-gone", "ghost"), source("agent-a", "alice")],
        ROSTER,
        PROJECT,
      ),
    ).toEqual([
      { id: "agent-a", name: "alice", projectId: PROJECT },
      { id: "agent-b", name: "bob", projectId: PROJECT },
    ]);
  });

  it("groups foreign sources after the locals, by project name then card order", () => {
    // A foreign agent is absent from the current project's roster by definition,
    // so a roster-membership pass alone would delete every cross-project chip.
    // They group instead: locals first in this roster's card order, then each
    // foreign project alphabetically, each in *its own* card order.
    const foreign = (
      id: string,
      name: string,
      projectId: string,
      projectName: string,
      rosterIndex: number,
    ): ForwardSource => ({ id, name, projectId, projectName, rosterIndex });
    // Declared in an order that fights the expected one on every axis: a
    // later-carded local first, project Z before project X, and each project's
    // second agent before its first.
    // Ids deliberately sort *opposite* to the names, so an implementation that
    // fell back to id order would fail rather than coincidentally agree.
    const picked = [
      foreign("z-2", "zeta-two", "p-aaa", "Zebra", 1),
      source("agent-b", "bob"),
      foreign("x-2", "xray-two", "p-zzz", "Xylo", 1),
      foreign("z-1", "zeta-one", "p-aaa", "Zebra", 0),
      source("agent-a", "alice"),
      foreign("x-1", "xray-one", "p-zzz", "Xylo", 0),
    ];
    expect(orderForwardSources(picked, ROSTER, PROJECT).map((s) => s.id)).toEqual([
      "agent-a",
      "agent-b",
      "x-1",
      "x-2",
      "z-1",
      "z-2",
    ]);
  });

  it("keeps two same-named projects from interleaving", () => {
    // Project names are user text and need not be unique; without a tiebreak
    // their agents would shuffle together under one apparent heading.
    const mk = (id: string, projectId: string, rosterIndex: number): ForwardSource => ({
      id,
      name: id,
      projectId,
      projectName: "Duplicate",
      rosterIndex,
    });
    const picked = [mk("b-1", "p-bbb", 0), mk("a-1", "p-aaa", 0), mk("b-0", "p-bbb", 1)];
    expect(orderForwardSources(picked, ROSTER, PROJECT).map((s) => s.id)).toEqual([
      "a-1",
      "b-1",
      "b-0",
    ]);
  });

  it("sorts a foreign agent with no known card position last within its project", () => {
    // A draft written before positions were captured, or one whose roster read
    // failed: it still renders, at the end of its own project's group.
    const known: ForwardSource = {
      id: "x-1",
      name: "xray-one",
      projectId: "p-xxx",
      projectName: "Xylo",
      rosterIndex: 0,
    };
    const unknown: ForwardSource = {
      id: "x-9",
      name: "xray-nine",
      projectId: "p-xxx",
      projectName: "Xylo",
    };
    expect(orderForwardSources([unknown, known], ROSTER, PROJECT).map((s) => s.id)).toEqual([
      "x-1",
      "x-9",
    ]);
  });

  it("expands pane members in agent-card order rather than pane insertion order", () => {
    const pane: TranscriptPane = {
      id: "pane-1",
      name: "reviewers",
      members: [BOB.id, ALICE.id],
      hidden: [],
    };
    expect(forwardSourceAgentsForPane(pane, ROSTER)).toEqual([
      { id: "agent-a", name: "alice", projectId: PROJECT },
      { id: "agent-b", name: "bob", projectId: PROJECT },
    ]);
  });
});

describe("reconcileForwardSources", () => {
  it("keeps surviving sources in the current roster order", () => {
    const sources = [source("agent-b", "bob"), source("agent-a", "alice")];
    expect(reconcileForwardSources(sources, ROSTER, PROJECT)).toEqual([
      { id: "agent-a", name: "alice", projectId: PROJECT },
      { id: "agent-b", name: "bob", projectId: PROJECT },
    ]);
  });

  it("drops a source whose agent was removed since the draft was written", () => {
    // Forwarding from a removed agent would fail at dispatch, so a restored draft
    // must not carry the chip forward.
    const sources = [source("agent-a", "alice"), source("agent-gone", "ghost")];
    expect(reconcileForwardSources(sources, ROSTER, PROJECT)).toEqual([
      { id: "agent-a", name: "alice", projectId: PROJECT },
    ]);
  });

  it("refreshes a renamed agent's display name from the roster", () => {
    // `name` is display-only. A stale one would label the chip with an agent name
    // that no longer exists anywhere in the UI.
    const sources = [source("agent-a", "old-name")];
    expect(reconcileForwardSources(sources, ROSTER, PROJECT)).toEqual([
      { id: "agent-a", name: "alice", projectId: PROJECT },
    ]);
  });

  it("returns nothing when the roster is empty", () => {
    expect(reconcileForwardSources([source("agent-a", "alice")], [], PROJECT)).toEqual([]);
  });

  it("returns nothing for no sources", () => {
    expect(reconcileForwardSources([], ROSTER, PROJECT)).toEqual([]);
  });
});

describe("reconcileForwardSourceMap", () => {
  it("reconciles each field independently", () => {
    const map = {
      focus: [source("agent-a", "stale")],
      context: [source("agent-b", "bob"), source("agent-a", "alice")],
    };
    expect(reconcileForwardSourceMap(map, ROSTER, PROJECT)).toEqual({
      focus: [{ id: "agent-a", name: "alice", projectId: PROJECT }],
      context: [
        { id: "agent-a", name: "alice", projectId: PROJECT },
        { id: "agent-b", name: "bob", projectId: PROJECT },
      ],
    });
  });

  it("drops a field whose every source is gone, rather than leaving it empty", () => {
    // An empty array would persist a key that means nothing and would make the
    // snapshot's "is this draft empty" check wrong.
    const map = {
      focus: [source("agent-gone", "ghost")],
      context: [source("agent-a", "alice")],
    };
    const out = reconcileForwardSourceMap(map, ROSTER, PROJECT);
    expect(out).toEqual({ context: [{ id: "agent-a", name: "alice", projectId: PROJECT }] });
    expect("focus" in out).toBe(false);
  });

  it("keeps a field that partially survives", () => {
    const map = { focus: [source("agent-gone", "ghost"), source("agent-b", "bob")] };
    expect(reconcileForwardSourceMap(map, ROSTER, PROJECT)).toEqual({
      focus: [{ id: "agent-b", name: "bob", projectId: PROJECT }],
    });
  });

  it("returns an empty map for an empty map", () => {
    expect(reconcileForwardSourceMap({}, ROSTER, PROJECT)).toEqual({});
  });
});

const agentTurn = (
  status: "streaming" | "complete" | "failed" | "cancelled",
  at: string,
  // A completed turn defaults to carrying answer text — readiness requires the
  // newest completed turn to have forwardable text, mirroring the backend's
  // `latest_completed_agent_text`.
  items: TurnItem[] = [{ item_kind: "text", kind: "text", text: "an answer" }],
): Turn => ({
  role: "agent",
  turn_id: `turn-${at}`,
  agent_id: "agent-a",
  started_at: at,
  status,
  items,
});

const userTurn = (at: string): Turn => ({
  role: "user",
  turn_id: `user-${at}`,
  agent_id: "agent-a",
  started_at: at,
  text: "hi",
});

describe("agentReadinessFor", () => {
  const rt = (
    run_status: "idle" | "starting" | "processing",
    hydration_status: "pending" | "loading" | "complete" | "failed",
  ) => ({ run_status, hydration_status });

  it("reports unknown, not empty, while the agent's history is still unread", () => {
    // Every agent is seeded with an empty transcript at registration, and a
    // failed read leaves it that way until the user retries hydration. Callers
    // *disable* on `empty`, so answering `empty` here makes an agent with months
    // of history unpickable — while asserting it has no output.
    expect(agentReadinessFor([], rt("idle", "loading"))).toBe("unknown");
    expect(agentReadinessFor(undefined, rt("idle", "failed"))).toBe("unknown");
  });

  it("still reports pending for a streaming turn the history read hasn't covered", () => {
    // A streaming turn arrives on the live event channel, not from disk, so it is
    // known regardless of hydration. Checking hydration first would hide an agent
    // that is visibly generating right now.
    expect(agentReadinessFor([agentTurn("streaming", "1")], rt("processing", "loading"))).toBe(
      "pending",
    );
  });

  it("reports pending, not empty, for a just-dispatched agent with no prior output", () => {
    // The just-sent window: run_status is `starting` but the first streamed
    // token hasn't reached the transcript. Deriving from turns alone reads
    // "no output" — a *disable* built on a false claim, at the exact moment a
    // user chains "send to A, forward A's reply to B".
    expect(agentReadinessFor([], rt("starting", "complete"))).toBe("pending");
  });

  it("reports pending, not ready, for a just-dispatched agent with an older answer", () => {
    // The backend holds for the new turn and forwards *its* output, so "ready"
    // (implying the old answer would be forwarded now) is the wrong promise.
    expect(agentReadinessFor([agentTurn("complete", "1")], rt("starting", "complete"))).toBe(
      "pending",
    );
  });

  it("withholds a stale ready verdict too, not just a stale empty one", () => {
    // A partially-read transcript can look ready while the newest turn is still
    // missing, so an incomplete read yields no verdict in either direction.
    expect(agentReadinessFor([agentTurn("complete", "1")], rt("idle", "loading"))).toBe("unknown");
  });

  it("treats a missing runtime as untrusted, not as idle-and-hydrated", () => {
    expect(agentReadinessFor([agentTurn("complete", "1")], undefined)).toBe("unknown");
  });

  it("passes the derivation straight through once the history is read", () => {
    expect(agentReadinessFor([], rt("idle", "complete"))).toBe("empty");
    expect(agentReadinessFor([agentTurn("complete", "1")], rt("idle", "complete"))).toBe("ready");
    expect(agentReadinessFor([agentTurn("streaming", "1")], rt("processing", "complete"))).toBe(
      "pending",
    );
  });
});

describe("forwardReadiness", () => {
  it("is empty for an agent with no turns", () => {
    expect(forwardReadiness([])).toBe("empty");
    expect(forwardReadiness(undefined)).toBe("empty");
  });

  it("is ready for an idle agent with a completed turn", () => {
    expect(forwardReadiness([agentTurn("complete", "1")])).toBe("ready");
  });

  it("is pending while a turn is streaming", () => {
    expect(forwardReadiness([agentTurn("streaming", "1")])).toBe("pending");
  });

  it("is pending for a completed turn followed by a newer streaming one", () => {
    // The forward awaits the in-flight turn and then takes the *latest* completed
    // output, so this agent is not ready — the send holds and forwards the new
    // turn, not the old one. A `hasCompleted || isStreaming` predicate says "ready"
    // here, which is the exact bug this function exists to prevent.
    expect(forwardReadiness([agentTurn("complete", "1"), agentTurn("streaming", "2")])).toBe(
      "pending",
    );
  });

  it("is ready when the newest turn failed or was cancelled (forwards the note)", () => {
    // The backend forwards a generated failure note for a failed/cancelled
    // latest turn — non-empty, deliberately forwardable ("tell the next agent
    // that X failed") — so the send succeeds and the chip must not claim it
    // would be blocked. (Inverts the old classification, which showed a
    // blocking warning on a path that dispatches fine.)
    expect(forwardReadiness([agentTurn("failed", "1")])).toBe("ready");
    expect(forwardReadiness([agentTurn("cancelled", "1")])).toBe("ready");
  });

  it("is ready when a later turn failed but an earlier one completed", () => {
    // Ready via the failure note (the backend forwards the *note*, not the
    // older completed text — latest-turn outcome wins).
    expect(forwardReadiness([agentTurn("complete", "1"), agentTurn("failed", "2")])).toBe("ready");
  });

  it("is empty for a textless completion even behind an older failure", () => {
    // Newest turn completed (no note applies) but carries no answer text.
    expect(forwardReadiness([agentTurn("failed", "1"), agentTurn("complete", "2", [])])).toBe(
      "empty",
    );
  });

  it("is empty for a completed turn with only tool/thinking items", () => {
    // The case the any-empty policy blocks at dispatch: completed, but no
    // answer text — the chip must warn instead of claiming ready.
    expect(
      forwardReadiness([
        agentTurn("complete", "1", [
          {
            item_kind: "tool",
            tool_use_id: "t1",
            kind: "builtin",
            name: "Bash",
            input: {},
            facet: { facet_kind: "other" },
            output: "did things",
            is_error: false,
            started_at: "1",
          },
          { item_kind: "text", kind: "thinking", text: "pondering" },
        ]),
      ]),
    ).toBe("empty");
  });

  it("is empty when the newest completion is textless despite an older textual one", () => {
    // The backend forwards the *newest* completed turn's text, so the older
    // answer does not make this source forwardable.
    expect(forwardReadiness([agentTurn("complete", "1"), agentTurn("complete", "2", [])])).toBe(
      "empty",
    );
  });

  it("ignores user turns", () => {
    // A user turn has no `status`; only agent turns carry forwardable output.
    expect(forwardReadiness([userTurn("1")])).toBe("empty");
    expect(forwardReadiness([userTurn("1"), agentTurn("complete", "2")])).toBe("ready");
  });
});
