import { describe, expect, it } from "vitest";
import { reconcileForwardSourceMap, reconcileForwardSources } from "./heldForwards.svelte";
import type { ForwardSource } from "./heldForwards.svelte";
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

const source = (id: string, name: string): ForwardSource => ({ id, name });

describe("reconcileForwardSources", () => {
  it("keeps sources whose agent is still on the roster, in order", () => {
    const sources = [source("agent-b", "bob"), source("agent-a", "alice")];
    expect(reconcileForwardSources(sources, ROSTER)).toEqual([
      { id: "agent-b", name: "bob" },
      { id: "agent-a", name: "alice" },
    ]);
  });

  it("drops a source whose agent was removed since the draft was written", () => {
    // Forwarding from a removed agent would fail at dispatch, so a restored draft
    // must not carry the chip forward.
    const sources = [source("agent-a", "alice"), source("agent-gone", "ghost")];
    expect(reconcileForwardSources(sources, ROSTER)).toEqual([{ id: "agent-a", name: "alice" }]);
  });

  it("refreshes a renamed agent's display name from the roster", () => {
    // `name` is display-only. A stale one would label the chip with an agent name
    // that no longer exists anywhere in the UI.
    const sources = [source("agent-a", "old-name")];
    expect(reconcileForwardSources(sources, ROSTER)).toEqual([{ id: "agent-a", name: "alice" }]);
  });

  it("returns nothing when the roster is empty", () => {
    expect(reconcileForwardSources([source("agent-a", "alice")], [])).toEqual([]);
  });

  it("returns nothing for no sources", () => {
    expect(reconcileForwardSources([], ROSTER)).toEqual([]);
  });
});

describe("reconcileForwardSourceMap", () => {
  it("reconciles each field independently", () => {
    const map = {
      focus: [source("agent-a", "stale")],
      context: [source("agent-b", "bob"), source("agent-a", "alice")],
    };
    expect(reconcileForwardSourceMap(map, ROSTER)).toEqual({
      focus: [{ id: "agent-a", name: "alice" }],
      context: [
        { id: "agent-b", name: "bob" },
        { id: "agent-a", name: "alice" },
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
    const out = reconcileForwardSourceMap(map, ROSTER);
    expect(out).toEqual({ context: [{ id: "agent-a", name: "alice" }] });
    expect("focus" in out).toBe(false);
  });

  it("keeps a field that partially survives", () => {
    const map = { focus: [source("agent-gone", "ghost"), source("agent-b", "bob")] };
    expect(reconcileForwardSourceMap(map, ROSTER)).toEqual({
      focus: [{ id: "agent-b", name: "bob" }],
    });
  });

  it("returns an empty map for an empty map", () => {
    expect(reconcileForwardSourceMap({}, ROSTER)).toEqual({});
  });
});
