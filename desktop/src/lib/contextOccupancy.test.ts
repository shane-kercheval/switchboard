import { readFileSync } from "node:fs";
import { resolve } from "node:path";

import { describe, expect, it } from "vitest";

import { contextOccupancy, type CompactedReading, type OccupancyTurn } from "./contextOccupancy";

type Case = {
  name: string;
  turns: OccupancyTurn[];
  compaction: CompactedReading | null;
  expected: { used_tokens: number; window_tokens: number; fraction: number } | null;
};

/// The cases agent access's copy of this rule runs too (`ops/context.rs`), so
/// the two cannot drift apart without one suite failing.
const fixture = JSON.parse(
  readFileSync(resolve(__dirname, "../../tests/fixtures/context-occupancy.json"), "utf8"),
) as { cases: Case[] };

describe("contextOccupancy, over the shared cases", () => {
  it("holds cases", () => {
    expect(fixture.cases.length).toBeGreaterThanOrEqual(16);
  });
  for (const c of fixture.cases) {
    it(c.name, () => {
      const got = contextOccupancy(c.turns, c.compaction ?? undefined);
      expect(
        got === undefined
          ? null
          : {
              used_tokens: got.usedTokens,
              window_tokens: got.windowTokens,
              fraction: got.fraction,
            },
      ).toEqual(c.expected);
    });
  }
});
