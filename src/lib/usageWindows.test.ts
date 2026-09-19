import { readFileSync } from "node:fs";
import { resolve } from "node:path";

import { describe, expect, it } from "vitest";
import { claudeRateLimitView, codexAccountUsageView } from "./usageWindows";

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

/// The account read's payload, shaped as Codex returns it. Field names are
/// camelCase here and snake_case in the rollout payload the old reader took —
/// they are different protocols, not a style choice.
///
/// `limitName: null` on the account-wide bucket is not an oversight in the
/// fixture: it is what every capture shows. Only the model reserve is named.
function accountPayload(buckets: Record<string, unknown>, ordinaryUsageAllowed = true): unknown {
  return { ordinaryUsageAllowed, rateLimitsByLimitId: buckets };
}

const ACCOUNT_BUCKET = {
  limitId: "codex",
  limitName: null,
  normalModelSlug: null,
  primary: { usedPercent: 42.0, windowDurationMins: 10080, resetsAt: future(86_400) },
  rateLimitReachedType: null,
};

const RESERVE_BUCKET = {
  limitId: "base_model_inference",
  limitName: "gpt-reserve",
  normalModelSlug: "gpt-5.6-luna",
  primary: { usedPercent: 5, windowDurationMins: 10080, resetsAt: future(86_400) },
  rateLimitReachedType: null,
};

describe("codexAccountUsageView selection", () => {
  it("renders an account-wide bucket", () => {
    const windows = codexAccountUsageView(accountPayload({ codex: ACCOUNT_BUCKET }), NOW);
    expect(windows).toHaveLength(1);
    expect(windows[0]?.key).toBe("codex");
    expect(windows[0]?.usedFraction).toBeCloseTo(0.42);
  });

  it("hides a model-specific reserve beside an account-wide quota", () => {
    // The whole point of reading the account: the reserve is a real quota with
    // real headroom, and showing it beside the allowance invites reading its
    // 95% remaining as the number that governs ordinary work.
    const windows = codexAccountUsageView(
      accountPayload({ codex: ACCOUNT_BUCKET, base_model_inference: RESERVE_BUCKET }),
      NOW,
    );
    expect(windows.map((w) => w.key)).toEqual(["codex"]);
  });

  it("skips a bucket that carries no `normalModelSlug` field at all", () => {
    // Absence is not null. Were the field renamed upstream, treating absence as
    // "no model" would promote every reserve into the account section and label
    // it as an ordinary allowance — the exact defect this reader replaced,
    // rebuilt in a new place. Rendering nothing is the safe direction.
    const { normalModelSlug: _dropped, ...noSlug } = ACCOUNT_BUCKET;
    expect(codexAccountUsageView(accountPayload({ codex: noSlug }), NOW)).toEqual([]);
  });

  it("labels an account-wide weekly quota in the harness-shared vocabulary", () => {
    // The same string Claude's `seven_day` row uses. Both halves are read
    // rather than assumed: the duration is stated by the payload, and "all
    // models" is what passing the `normalModelSlug === null` filter *means*.
    // The old reader's version of this label was an invention because it could
    // not tell the account allowance from the model reserve; this one only ever
    // labels buckets that are provably the former.
    const windows = codexAccountUsageView(accountPayload({ codex: ACCOUNT_BUCKET }), NOW);
    expect(windows[0]?.label).toBe("Weekly · all models");
  });

  it("labels an account-wide 5-hour quota with the shared string too", () => {
    const fiveHour = {
      ...ACCOUNT_BUCKET,
      primary: { usedPercent: 12, windowDurationMins: 300, resetsAt: future(1800) },
    };
    expect(codexAccountUsageView(accountPayload({ codex: fiveHour }), NOW)[0]?.label).toBe(
      "5-hour limit",
    );
  });

  it("falls back to Codex's own name for a duration we do not recognize", () => {
    const odd = {
      ...ACCOUNT_BUCKET,
      limitName: "Monthly",
      primary: { usedPercent: 12, windowDurationMins: 43_200, resetsAt: future(86_400) },
    };
    expect(codexAccountUsageView(accountPayload({ codex: odd }), NOW)[0]?.label).toBe("Monthly");
  });

  it("falls back to a neutral noun when there is nothing to name it with", () => {
    // No recognized duration and no name: say less rather than invent either.
    const bare = { ...ACCOUNT_BUCKET, primary: { usedPercent: 12, resetsAt: future(86_400) } };
    expect(codexAccountUsageView(accountPayload({ codex: bare }), NOW)[0]?.label).toBe("Quota");
  });
});

/// The reader run against the **real** captured response, rather than against
/// fixtures shaped by hand from the same understanding that wrote the reader.
///
/// The capture is the one taken the moment the account's weekly window rolled
/// over; it is checked in under the harness crate because that is where the
/// Rust read's own tests consume it. Reading the same bytes from both sides is
/// the point — a hand-built fixture agreeing with the code that reads it proves
/// only that they were written together.
describe("codexAccountUsageView against the recorded account response", () => {
  const captured = JSON.parse(
    readFileSync(
      resolve(
        process.cwd(),
        "crates/harness/tests/fixtures/codex/account-rate-limits-healthy.jsonl",
      ),
      "utf8",
    )
      .split("\n")
      .find((line) => line.includes('"id":1'))!,
  ).result as { rateLimitsByLimitId: Record<string, { primary: { resetsAt: number } }> };

  // An hour before the account-wide bucket's reset, so the reset-passed rule
  // does not retire the capture as the recorded timestamps recede into the past.
  const justBeforeReset = (captured.rateLimitsByLimitId.codex!.primary.resetsAt - 3600) * 1000;

  it("renders the account-wide quota and hides the model reserve", () => {
    const windows = codexAccountUsageView(captured, justBeforeReset);
    expect(windows.map((w) => w.key)).toEqual(["codex"]);
  });

  it("reads the recovered account as healthy, labelled from its real window", () => {
    // Properties of the real payload rather than of the fixture author: the
    // account-wide bucket carries a 10080-minute window and `limitName: null`,
    // so the shared label has to come from the duration; and a recovered window
    // clears its exhaustion flag.
    const [quota] = codexAccountUsageView(captured, justBeforeReset);
    expect(quota?.label).toBe("Weekly · all models");
    expect(quota?.limitReached).toBeUndefined();
    expect(quota?.usedFraction).toBe(0);
  });
});

describe("codexAccountUsageView input validation", () => {
  it.each([
    ["a non-object payload", "nope"],
    ["null", null],
    ["a payload with no bucket map", { ordinaryUsageAllowed: true }],
    ["a null bucket map", { rateLimitsByLimitId: null }],
    ["a non-object bucket map", { rateLimitsByLimitId: "nope" }],
    ["an empty bucket map", { rateLimitsByLimitId: {} }],
  ])("returns an empty list for %s", (_case, payload) => {
    expect(codexAccountUsageView(payload, NOW)).toEqual([]);
  });

  it("skips a bucket whose window is absent", () => {
    // Observed: `limit_id: "premium"` arrives with both windows null on a
    // refused turn. A bar with no value is worse than no bar.
    const windowless = { ...ACCOUNT_BUCKET, primary: null };
    expect(codexAccountUsageView(accountPayload({ premium: windowless }), NOW)).toEqual([]);
  });

  it("skips a bucket whose percentage is not a number", () => {
    const bad = { ...ACCOUNT_BUCKET, primary: { usedPercent: "42", resetsAt: future(86_400) } };
    expect(codexAccountUsageView(accountPayload({ codex: bad }), NOW)).toEqual([]);
  });

  it("keeps a bucket that reports no reset", () => {
    // Without a reset there is no way to prove the percentage stale, so it
    // stands — the same rule the Claude reader follows.
    const noReset = { ...ACCOUNT_BUCKET, primary: { usedPercent: 42.0 } };
    const windows = codexAccountUsageView(accountPayload({ codex: noReset }), NOW);
    expect(windows).toHaveLength(1);
    expect(windows[0]?.resetsAtMs).toBeNull();
  });

  it("drops a bucket whose reset has passed while its siblings stay", () => {
    const cycled = {
      ...ACCOUNT_BUCKET,
      limitId: "stale",
      primary: { usedPercent: 100.0, resetsAt: future(-60) },
    };
    const live = { ...ACCOUNT_BUCKET, limitId: "live" };
    const windows = codexAccountUsageView(accountPayload({ stale: cycled, live }), NOW);
    expect(windows.map((w) => w.key)).toEqual(["live"]);
  });
});

describe("codexAccountUsageView exhaustion", () => {
  it("draws a bucket as exhausted because Codex says so, without touching the measurement", () => {
    // The measurement stands as measured. The reader this replaced painted its
    // guessed window to 100%, because the per-turn payload recorded no
    // measurement on a refusal and the stale number would have contradicted the
    // refusal beside it. The account read reports both, so neither is invented.
    const capped = {
      ...ACCOUNT_BUCKET,
      primary: { usedPercent: 100.0, windowDurationMins: 10080, resetsAt: future(86_400) },
      rateLimitReachedType: "rate_limit_reached",
    };
    const windows = codexAccountUsageView(accountPayload({ codex: capped }, false), NOW);
    expect(windows[0]?.limitReached).toBe(true);
    expect(windows[0]?.usedFraction).toBe(1);
  });

  it("flags only the bucket that reports itself exhausted", () => {
    // The old reader had to guess which window a refusal belonged to, because
    // Codex named none. Each bucket now states its own, so a spent 5-hour quota
    // cannot condemn the weekly one.
    const capped = {
      ...ACCOUNT_BUCKET,
      limitId: "five_hour",
      primary: { usedPercent: 100.0, resetsAt: future(1800) },
      rateLimitReachedType: "rate_limit_reached",
    };
    const windows = codexAccountUsageView(
      accountPayload({ five_hour: capped, weekly: { ...ACCOUNT_BUCKET, limitId: "weekly" } }),
      NOW,
    );
    expect(windows.map((w) => [w.key, w.limitReached])).toEqual([
      ["five_hour", true],
      ["weekly", undefined],
    ]);
  });

  it("ignores a stale exhaustion flag on a bucket reporting nothing spent", () => {
    // A quota 0% spent and simultaneously exhausted is a contradiction, so the
    // measurement wins and nothing is claimed. A probe against a recovered
    // account found the flag clears on reset, so this guards a shape Codex does
    // not currently produce — kept because it is free.
    const contradictory = {
      ...ACCOUNT_BUCKET,
      primary: { usedPercent: 0, resetsAt: future(86_400) },
      rateLimitReachedType: "rate_limit_reached",
    };
    const windows = codexAccountUsageView(accountPayload({ codex: contradictory }), NOW);
    expect(windows[0]?.limitReached).toBeUndefined();
  });

  it("reads a recovered account as healthy", () => {
    // The shape captured the moment the weekly window rolled over: a new
    // reset, nothing spent, and the flag back to null.
    const windows = codexAccountUsageView(accountPayload({ codex: ACCOUNT_BUCKET }), NOW);
    expect(windows[0]?.limitReached).toBeUndefined();
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
