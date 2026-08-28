import { describe, expect, it } from "vitest";
import {
  expandForwardSources,
  forwardReadiness,
  forwardSourceLabel,
  reconcileForwardSourceMap,
  reconcileForwardSources,
} from "./heldForwards.svelte";
import type { ForwardSource } from "./heldForwards.svelte";
import type { Turn, TurnItem } from "./types";
import type { AgentRecord } from "$lib/types";

const agent = (id: string, name: string): AgentRecord => ({
  id,
  project_id: "00000000-0000-7000-8000-0000000000ff",
  name,
  harness: "claude_code",
  session_locator: null,
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
    expect(forwardSourceLabel(foreign, PROJECT)).toBe("alice · backend");
  });

  it("falls back to the bare name when a foreign project's name is unknown", () => {
    const foreign: ForwardSource = { id: "agent-z", name: "oracle", projectId: OTHER };
    expect(forwardSourceLabel(foreign, PROJECT)).toBe("oracle");
  });
});

describe("reconcileForwardSources", () => {
  it("keeps sources whose agent is still on the roster, in order", () => {
    const sources = [source("agent-b", "bob"), source("agent-a", "alice")];
    expect(reconcileForwardSources(sources, ROSTER, PROJECT)).toEqual([
      { id: "agent-b", name: "bob", projectId: PROJECT },
      { id: "agent-a", name: "alice", projectId: PROJECT },
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
        { id: "agent-b", name: "bob", projectId: PROJECT },
        { id: "agent-a", name: "alice", projectId: PROJECT },
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
