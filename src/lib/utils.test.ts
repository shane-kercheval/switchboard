import { describe, it, expect } from "vitest";
import { basename, pickNewestAgent } from "./utils";
import type { AgentRecord } from "./types";

function agent(id: string, name: string, created_at: string): AgentRecord {
  return {
    id,
    project_id: "00000000-0000-7000-8000-000000000000",
    name,
    harness: "claude_code",
    session_id: null,
    created_at,
  };
}

describe("basename", () => {
  it("returns the last path component for an absolute path", () => {
    expect(basename("/Users/x/repos/temp")).toBe("temp");
  });

  it("trims a single trailing slash", () => {
    expect(basename("/Users/x/repos/temp/")).toBe("temp");
  });

  it("returns the input when there is no slash", () => {
    expect(basename("just-a-name")).toBe("just-a-name");
  });

  it("handles dot-prefixed components", () => {
    expect(basename("/Users/x/.switchboard")).toBe(".switchboard");
  });
});

describe("pickNewestAgent", () => {
  it("picks the agent with the latest created_at", () => {
    const a = agent("a", "first", "2026-05-13T00:00:00Z");
    const b = agent("b", "second", "2026-05-13T01:00:00Z");
    const c = agent("c", "third", "2026-05-13T00:30:00Z");
    expect(pickNewestAgent([a, b, c]).id).toBe("b");
  });

  it("tiebreaks deterministically by id desc when timestamps are identical", () => {
    // Same created_at means the sort must fall back to id; the higher id wins.
    const a = agent("aaaa", "a", "2026-05-13T00:00:00Z");
    const b = agent("zzzz", "z", "2026-05-13T00:00:00Z");
    expect(pickNewestAgent([a, b]).id).toBe("zzzz");
    expect(pickNewestAgent([b, a]).id).toBe("zzzz");
  });

  it("does not mutate the input array", () => {
    const a = agent("a", "first", "2026-05-13T00:00:00Z");
    const b = agent("b", "second", "2026-05-13T01:00:00Z");
    const input = [a, b];
    pickNewestAgent(input);
    expect(input).toEqual([a, b]);
  });

  it("throws on empty input", () => {
    expect(() => pickNewestAgent([])).toThrow();
  });

  it("orders correctly across timezone-suffix variations (Z vs +00:00 vs offsets)", () => {
    // Regression: lexical comparison of ISO 8601 strings only works when
    // every record carries the same timezone suffix. Real wall-clock-ordered
    // records here, with the same instants but different surface formats.
    const earlier = agent("a", "earlier", "2026-05-13T00:00:00Z"); // T=0
    const middle = agent("b", "middle", "2026-05-13T01:00:00+00:00"); // T=+1h (same instant as below)
    const later = agent("c", "later", "2026-05-13T03:00:00.000+02:00"); // T=+1h (same as middle)
    const latest = agent("d", "latest", "2026-05-13T02:00:00Z"); // T=+2h, the newest
    expect(pickNewestAgent([earlier, middle, later, latest]).id).toBe("d");
  });
});
