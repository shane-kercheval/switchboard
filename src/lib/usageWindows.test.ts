import { describe, expect, it } from "vitest";
import { claudeRateLimitView, codexRateLimitView, sameCodexUsageWindows } from "./usageWindows";

/// Input validation against each harness's opaque payload. These live here
/// rather than in the Sidebar suite because they are about what the derivation
/// *accepts*, which a rendered card can only show as an absent meter — the
/// rendering claims (meter counts, labels, order, tone, fallback) stay at the
/// component level where they can actually be seen.
const NOW = Date.UTC(2026, 8, 17, 12, 0, 0);
const future = (seconds: number): number => Math.floor(NOW / 1000) + seconds;

function claudeWindows(fiveHour: unknown): ReturnType<typeof claudeRateLimitView> {
  return claudeRateLimitView(
    {
      status: "allowed",
      unifiedWindows: {
        five_hour: fiveHour,
        seven_day: { utilization: 0.27, resetsAt: future(5 * 86400) },
      },
    },
    NOW,
    undefined,
  );
}

describe("claudeRateLimitView input validation", () => {
  it.each([
    ["a string utilization", { utilization: "0.5", resetsAt: future(3600) }],
    ["a utilization above 1", { utilization: 1.5, resetsAt: future(3600) }],
    ["a negative utilization", { utilization: -0.2, resetsAt: future(3600) }],
    ["no utilization", { resetsAt: future(3600) }],
    ["no reset", { utilization: 0.33 }],
    ["a string reset", { utilization: 0.33, resetsAt: "soon" }],
    ["a non-object entry", "five_hour"],
    ["a null entry", null],
  ])("drops a window with %s and keeps its sibling", (_case, fiveHour) => {
    // An unreadable window is not a nearly-empty one: a meter drawn from it
    // would state a number the harness never sent.
    const view = claudeWindows(fiveHour);
    expect(view?.windows.map((w) => w.key)).toEqual(["seven_day"]);
  });

  it("keeps a window whose fraction sits exactly on either bound", () => {
    // 0 and 1 are real readings — an empty window and a spent one.
    expect(
      claudeWindows({ utilization: 0, resetsAt: future(3600) })?.windows[0]?.usedFraction,
    ).toBe(0);
    expect(
      claudeWindows({ utilization: 1, resetsAt: future(3600) })?.windows[0]?.usedFraction,
    ).toBe(1);
  });

  it.each([
    ["a non-object payload", "nope"],
    ["null", null],
    ["a payload with nothing displayable", { status: "allowed" }],
  ])("returns null for %s", (_case, payload) => {
    expect(claudeRateLimitView(payload, NOW, undefined)).toBeNull();
  });

  it("reads the container as absent when it is empty, so the fallback applies", () => {
    const view = claudeRateLimitView(
      { status: "allowed", rateLimitType: "five_hour", resetsAt: future(3600), unifiedWindows: {} },
      NOW,
      undefined,
    );
    expect(view?.windows).toEqual([]);
    expect(view?.fallback?.label).toBe("5-hour limit");
  });
});

describe("codexRateLimitView input validation", () => {
  it("skips a window with a non-numeric percentage", () => {
    const windows = codexRateLimitView(
      { primary: { used_percent: "42", window_minutes: 300 }, secondary: { used_percent: 7.0 } },
      NOW,
    );
    expect(windows).toHaveLength(1);
    expect(windows[0]?.usedFraction).toBeCloseTo(0.07);
  });

  it("keeps a window that reports no reset", () => {
    // Older Codex shapes and minimal fixtures omit `resets_at`; without one we
    // cannot prove the percentage is stale, so it is shown.
    const windows = codexRateLimitView({ primary: { used_percent: 42.0 } }, NOW);
    expect(windows).toHaveLength(1);
    expect(windows[0]?.resetsAtMs).toBeNull();
  });

  it.each([
    ["a non-object payload", "nope"],
    ["null", null],
    ["a payload with neither window", { credits: null }],
  ])("returns an empty list for %s", (_case, payload) => {
    expect(codexRateLimitView(payload, NOW)).toEqual([]);
  });

  it("draws the refused window full and flagged once the harness has refused a turn", () => {
    // The snapshot still holds the last measurement — Codex records a
    // windowless payload on the refused turn, which is kept out of it — so the
    // refusal is the only thing that can say the window is actually used up.
    const windows = codexRateLimitView(
      { primary: { used_percent: 93.0, window_minutes: 10080, resets_at: future(86_400) } },
      NOW,
      true,
    );
    expect(windows).toHaveLength(1);
    expect(windows[0]?.usedFraction).toBe(1);
    expect(windows[0]?.limitReached).toBe(true);
    expect(windows[0]?.label).toBe("Weekly · all models");
  });

  it("attributes a refusal to the most-used window and leaves the others measured", () => {
    // Codex names no window, so flagging both would claim the weekly quota is
    // gone when only the 5-hour one is — days of waiting for an hour of it.
    const windows = codexRateLimitView(
      {
        primary: { used_percent: 99.0, window_minutes: 300, resets_at: future(1800) },
        secondary: { used_percent: 30.0, window_minutes: 10080, resets_at: future(86_400) },
      },
      NOW,
      true,
    );
    expect(windows.map((w) => [w.label, w.usedFraction, w.limitReached])).toEqual([
      ["5-hour limit", 1, true],
      ["Weekly · all models", 0.3, undefined],
    ]);
  });

  it("attributes to the weekly window when that is the fuller one", () => {
    // Same payload shape, opposite usage — the attribution follows the
    // measurement rather than the key order.
    const windows = codexRateLimitView(
      {
        primary: { used_percent: 12.0, window_minutes: 300, resets_at: future(1800) },
        secondary: { used_percent: 97.0, window_minutes: 10080, resets_at: future(86_400) },
      },
      NOW,
      true,
    );
    expect(windows.map((w) => [w.label, w.usedFraction, w.limitReached])).toEqual([
      ["5-hour limit", 0.12, undefined],
      ["Weekly · all models", 1, true],
    ]);
  });

  it("leaves the measurement alone when no turn has been refused", () => {
    const windows = codexRateLimitView(
      { primary: { used_percent: 93.0, window_minutes: 10080, resets_at: future(86_400) } },
      NOW,
      false,
    );
    expect(windows[0]?.usedFraction).toBeCloseTo(0.93);
    expect(windows[0]?.limitReached).toBeUndefined();
  });

  it("still drops a cycled window after a refusal — the refusal is as stale as the window", () => {
    const windows = codexRateLimitView(
      { primary: { used_percent: 100.0, window_minutes: 10080, resets_at: future(-60) } },
      NOW,
      true,
    );
    expect(windows).toEqual([]);
  });
});

describe("sameCodexUsageWindows", () => {
  const weekly = {
    primary: { used_percent: 93.0, window_minutes: 10080, resets_at: 1_789_845_487 },
  };

  it("matches a payload against itself, so a refusal survives its own turn end", () => {
    expect(sameCodexUsageWindows(weekly, { ...weekly })).toBe(true);
  });

  it("ignores the measured percentage — only the window matters", () => {
    const later = {
      primary: { used_percent: 99.0, window_minutes: 10080, resets_at: 1_789_845_487 },
    };
    expect(sameCodexUsageWindows(weekly, later)).toBe(true);
  });

  it("is order-independent across the two window keys", () => {
    const a = {
      primary: { used_percent: 10, resets_at: 200 },
      secondary: { used_percent: 20, resets_at: 100 },
    };
    const b = {
      primary: { used_percent: 10, resets_at: 100 },
      secondary: { used_percent: 20, resets_at: 200 },
    };
    expect(sameCodexUsageWindows(a, b)).toBe(true);
  });

  it("separates a rolled window from the one before it", () => {
    const rolled = {
      primary: { used_percent: 2.0, window_minutes: 10080, resets_at: 1_790_375_461 },
    };
    expect(sameCodexUsageWindows(weekly, rolled)).toBe(false);
  });

  it("separates payloads that report a different number of windows", () => {
    const both = { ...weekly, secondary: { used_percent: 4.0, resets_at: 1_790_000_000 } };
    expect(sameCodexUsageWindows(weekly, both)).toBe(false);
  });

  it.each([
    ["a window with no reset time", { primary: { used_percent: 93.0 } }],
    ["a windowless payload", { limit_id: "premium", primary: null, secondary: null }],
    ["a non-object", "nope"],
    ["null", null],
  ])("reports %s as not-the-same, in either position", (_case, indeterminate) => {
    // Unknown counts as different: two reset-less windows are indistinguishable
    // and the reset-passed gate can never retire one, so treating them as equal
    // would strand a refusal on a window forever.
    expect(sameCodexUsageWindows(weekly, indeterminate)).toBe(false);
    expect(sameCodexUsageWindows(indeterminate, weekly)).toBe(false);
    expect(sameCodexUsageWindows(indeterminate, indeterminate)).toBe(false);
  });
});

/// Claude's hard wall, verbatim from a rollout that hit the Fable weekly cap on
/// 2026-09-18. The account-wide windows still had room, which is why the refusal
/// has to land on one window rather than on the harness.
describe("claudeRateLimitView at the wall", () => {
  const rejected = {
    status: "rejected",
    rateLimitType: "seven_day_overage_included",
    isUsingOverage: false,
    unifiedWindows: {
      five_hour: { utilization: 0.28, resetsAt: future(3600) },
      seven_day: { utilization: 0.7, resetsAt: future(5 * 86400) },
      seven_day_overage_included: { utilization: 1, resetsAt: future(5 * 86400) },
    },
  };

  it("flags the window the payload names and leaves its siblings measured", () => {
    const view = claudeRateLimitView(rejected, NOW, "claude-fable-5-1");
    expect(view?.windows.map((w) => [w.label, w.usedFraction, w.limitReached])).toEqual([
      ["5-hour limit", 0.28, undefined],
      ["Weekly · all models", 0.7, undefined],
      ["Weekly · Fable", 1, true],
    ]);
  });

  it("does not overwrite the measurement, unlike the Codex reader", () => {
    // Claude reports the blocked window's real utilization in the same payload
    // that refuses, so there is nothing to infer. A reading below the cap stays
    // below it and is flagged, rather than being rounded up to a full bar.
    const belowCap = {
      ...rejected,
      unifiedWindows: {
        seven_day_overage_included: { utilization: 0.97, resetsAt: future(5 * 86400) },
      },
    };
    const view = claudeRateLimitView(belowCap, NOW, "claude-fable-5-1");
    expect(view?.windows[0]?.usedFraction).toBe(0.97);
    expect(view?.windows[0]?.limitReached).toBe(true);
  });

  it("leaves every window unflagged while the status is allowed", () => {
    const view = claudeRateLimitView({ ...rejected, status: "allowed" }, NOW, "claude-fable-5-1");
    expect(view?.windows.every((w) => w.limitReached === undefined)).toBe(true);
  });

  it("flags nothing when the refusal names a window the reader drops", () => {
    // Same rule the threshold warning already follows: an unrecognized key is
    // dropped rather than labelled by guesswork, and its flag goes with it.
    const view = claudeRateLimitView(
      { ...rejected, rateLimitType: "seven_day_something_new" },
      NOW,
      "claude-fable-5-1",
    );
    expect(view?.windows.every((w) => w.limitReached === undefined)).toBe(true);
  });
});

describe("claudeRateLimitView on an overage turn", () => {
  /// `rejected` is the same status the hard wall uses, but an overage turn is
  /// *served*: the quota is spent and Anthropic bills credits for work it still
  /// does. Recorded shape, harness-behavior.md §1.4.
  const overaging = {
    status: "rejected",
    rateLimitType: "seven_day",
    isUsingOverage: true,
    overageResetsAt: future(6 * 86400),
    unifiedWindows: {
      five_hour: { utilization: 0.3, resetsAt: future(3600) },
      seven_day: { utilization: 1, resetsAt: future(5 * 86400) },
    },
  };

  it("leaves the spent window unflagged, so the tone stays neutral", () => {
    // Flagging it would keep the card permanently amber for anyone routinely in
    // overage, and make a genuine refusal indistinguishable from being billed.
    const view = claudeRateLimitView(overaging, NOW, undefined);
    expect(view?.windows.every((w) => w.limitReached === undefined)).toBe(true);
  });

  it("still reports the credits escalation, which is the signal for this state", () => {
    const view = claudeRateLimitView(overaging, NOW, undefined);
    expect(view?.overage).not.toBeNull();
  });

  it("flags the window again once the same status arrives without overage", () => {
    // The discriminator is the overage flag, not the status: the captured wall
    // carries `isUsingOverage: false`.
    const view = claudeRateLimitView({ ...overaging, isUsingOverage: false }, NOW, undefined);
    expect(view?.windows.find((w) => w.key === "seven_day")?.limitReached).toBe(true);
  });
});
