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
function accountPayload(buckets: Record<string, unknown>): unknown {
  return { ordinaryUsageAllowed: true, rateLimitsByLimitId: buckets };
}

/// Windows are built from the schema's shape rather than from the shape this
/// account happens to produce. Both live captures carry `secondary: null`, so a
/// fixture derived from them cannot exercise the two-window bucket that other
/// plans do produce — which is exactly how the second slot went unread.
function window(usedPercent: number, windowDurationMins: number, resetsIn = 86_400): unknown {
  return { usedPercent, windowDurationMins, resetsAt: future(resetsIn) };
}

const ACCOUNT_BUCKET = {
  limitId: "codex",
  limitName: null,
  normalModelSlug: null,
  primary: window(42, 10080),
  secondary: null,
};

const RESERVE_BUCKET = {
  limitId: "base_model_inference",
  limitName: "gpt-reserve",
  normalModelSlug: "gpt-5.6-luna",
  primary: window(5, 10080),
  secondary: null,
};

const view = (buckets: Record<string, unknown>): ReturnType<typeof codexAccountUsageView> =>
  codexAccountUsageView(accountPayload(buckets), NOW);

describe("codexAccountUsageView bucket selection", () => {
  it("renders an account-wide bucket", () => {
    const { windows } = view({ codex: ACCOUNT_BUCKET });
    expect(windows).toHaveLength(1);
    expect(windows[0]?.usedFraction).toBeCloseTo(0.42);
  });

  it("hides a model-specific reserve beside an account-wide quota", () => {
    // The whole point of reading the account: the reserve is a real quota with
    // real headroom, and showing it beside the allowance invites reading its
    // 95% remaining as the number that governs ordinary work.
    const { windows } = view({ codex: ACCOUNT_BUCKET, base_model_inference: RESERVE_BUCKET });
    expect(windows.map((w) => w.key)).toEqual(["codex:primary"]);
  });

  it("skips a bucket that carries no `normalModelSlug` field, and reports it", () => {
    // Absence is not null. Every field on a bucket is optional in the protocol
    // schema, so a server that stops emitting nulls produces this on a valid
    // response — treating it as "no model" would promote every reserve into the
    // account section. Reporting is what keeps that from emptying the panel in
    // silence.
    const { normalModelSlug: _dropped, ...noSlug } = ACCOUNT_BUCKET;
    const { windows, diagnostics } = view({ codex: noSlug });
    expect(windows).toEqual([]);
    expect(diagnostics).toEqual([{ kind: "bucket-without-model-association", limitId: "codex" }]);
  });

  it("reports nothing for an account whose only quota is a model reserve", () => {
    // Legitimately empty, not drift: nothing failed to read, there is simply
    // no account-wide allowance to draw.
    const { windows, diagnostics } = view({ base_model_inference: RESERVE_BUCKET });
    expect(windows).toEqual([]);
    expect(diagnostics).toEqual([]);
  });
});

describe("codexAccountUsageView bucket windows", () => {
  it("renders both windows of one bucket", () => {
    // A bucket is a container of up to two windows for the *same* limit,
    // typically a short one and a weekly one. Reading only `primary` drops the
    // second quota entirely.
    const { windows } = view({
      codex: { ...ACCOUNT_BUCKET, primary: window(12, 300, 1800), secondary: window(70, 10080) },
    });
    expect(windows.map((w) => [w.key, w.label])).toEqual([
      ["codex:primary", "5-hour limit"],
      ["codex:secondary", "Weekly · all models"],
    ]);
  });

  it("renders a bucket that reports only its secondary window", () => {
    const { windows } = view({
      codex: { ...ACCOUNT_BUCKET, primary: null, secondary: window(70, 10080) },
    });
    expect(windows.map((w) => w.key)).toEqual(["codex:secondary"]);
  });

  it("expires a bucket's windows independently", () => {
    // The failure this guards: the short window cycles, and with one window per
    // bucket the whole row vanishes while the weekly quota is still capped.
    const { windows } = view({
      codex: {
        ...ACCOUNT_BUCKET,
        primary: window(100, 300, -60),
        secondary: window(98, 10080),
      },
    });
    expect(windows.map((w) => w.key)).toEqual(["codex:secondary"]);
    expect(windows[0]?.usedFraction).toBeCloseTo(0.98);
  });

  it("reports a window whose percentage is unreadable", () => {
    const bad = { ...ACCOUNT_BUCKET, primary: { usedPercent: "42", resetsAt: future(86_400) } };
    const { windows, diagnostics } = view({ codex: bad });
    expect(windows).toEqual([]);
    expect(diagnostics).toEqual([
      { kind: "window-without-usable-percent", limitId: "codex", slot: "primary" },
    ]);
  });

  it("keeps a window that reports no reset", () => {
    // Without a reset there is no way to prove the percentage stale, so it
    // stands — the same rule the Claude reader follows.
    const noReset = { ...ACCOUNT_BUCKET, primary: { usedPercent: 42, windowDurationMins: 10080 } };
    const { windows } = view({ codex: noReset });
    expect(windows).toHaveLength(1);
    expect(windows[0]?.resetsAtMs).toBeNull();
  });

  it("skips a windowless bucket", () => {
    // Observed on a refused turn: a bucket arrives with both slots null.
    const { windows } = view({ premium: { ...ACCOUNT_BUCKET, primary: null, secondary: null } });
    expect(windows).toEqual([]);
  });
});

describe("codexAccountUsageView exhaustion", () => {
  it("marks a spent quota from the measurement alone", () => {
    // Codex also sends a reason code naming why a limit was hit, and this reader
    // deliberately ignores it: four of its five values are team-billing states
    // that say nothing about a usage window, and deciding which bar the fifth
    // applied to produced a defect in two consecutive review rounds. 100% used
    // is what fills the bar and what marks it.
    const { windows } = view({ codex: { ...ACCOUNT_BUCKET, primary: window(100, 10080) } });
    expect(windows[0]?.usedFraction).toBe(1);
    expect(windows[0]?.limitReached).toBe(true);
  });

  it("leaves a quota below 100% unmarked", () => {
    const { windows } = view({ codex: { ...ACCOUNT_BUCKET, primary: window(99, 10080) } });
    expect(windows[0]?.limitReached).toBeUndefined();
  });

  it("ignores the reason code entirely", () => {
    // Same payload with and without every reason code Codex can send; the
    // rendered rows are identical. This is what keeps a rename or a sixth value
    // upstream from changing anything the user sees.
    const plain = view({ codex: { ...ACCOUNT_BUCKET, primary: window(100, 10080) } }).windows;
    for (const reason of [
      "rate_limit_reached",
      "workspace_owner_credits_depleted",
      "workspace_member_credits_depleted",
      "workspace_owner_usage_limit_reached",
      "workspace_member_usage_limit_reached",
      "some_future_value",
    ]) {
      const withReason = view({
        codex: { ...ACCOUNT_BUCKET, primary: window(100, 10080), rateLimitReachedType: reason },
      }).windows;
      expect(withReason).toEqual(plain);
    }
  });
});

describe("codexAccountUsageView labels", () => {
  it("labels an account-wide weekly quota in the harness-shared vocabulary", () => {
    // The same string Claude's `seven_day` row uses. Both halves are read
    // rather than assumed: the duration is stated by the payload, and "all
    // models" is what passing the `normalModelSlug === null` filter *means*.
    expect(view({ codex: ACCOUNT_BUCKET }).windows[0]?.label).toBe("Weekly · all models");
  });

  it("labels an account-wide 5-hour quota with the shared string too", () => {
    const fiveHour = { ...ACCOUNT_BUCKET, primary: window(12, 300, 1800) };
    expect(view({ codex: fiveHour }).windows[0]?.label).toBe("5-hour limit");
  });

  it("falls back to Codex's own name for a duration we do not recognize", () => {
    const odd = { ...ACCOUNT_BUCKET, limitName: "Monthly", primary: window(12, 43_200) };
    expect(view({ codex: odd }).windows[0]?.label).toBe("Monthly");
  });

  it("falls back to a neutral noun when there is nothing to name it with", () => {
    const bare = { ...ACCOUNT_BUCKET, primary: { usedPercent: 12, resetsAt: future(86_400) } };
    expect(view({ codex: bare }).windows[0]?.label).toBe("Quota");
  });

  it("leaves two same-duration account quotas sharing a label, distinct by key", () => {
    // Pins today's behaviour rather than disambiguating: no supported plan
    // produces two account-wide windows of equal length, and the identifiers a
    // fallback would reach for are shared by a bucket's own two windows. If
    // this ever fires in the wild, the rows are still separable by reset time.
    const { windows } = view({ a: { ...ACCOUNT_BUCKET }, b: { ...ACCOUNT_BUCKET, limitId: "b" } });
    expect(windows.map((w) => w.label)).toEqual(["Weekly · all models", "Weekly · all models"]);
    expect(new Set(windows.map((w) => w.key)).size).toBe(2);
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
///
/// What it **cannot** catch is a shape this account's plan does not produce.
/// Both captures carry `secondary: null`, which is how the second window slot
/// went unread; the fixtures above are built from the schema for that reason.
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
    const { windows } = codexAccountUsageView(captured, justBeforeReset);
    expect(windows.map((w) => w.key)).toEqual(["codex:primary"]);
  });

  it("reads the recovered account as healthy, labelled from its real window", () => {
    // Properties of the real payload rather than of the fixture author: the
    // account-wide bucket carries a 10080-minute window and `limitName: null`,
    // so the shared label has to come from the duration; and a recovered window
    // clears its exhaustion flag.
    const [quota] = codexAccountUsageView(captured, justBeforeReset).windows;
    expect(quota?.label).toBe("Weekly · all models");
    expect(quota?.limitReached).toBeUndefined();
    expect(quota?.usedFraction).toBe(0);
  });

  it("reports nothing unreadable in a real response", () => {
    // The drift detector, against real bytes: if Codex stops emitting the field
    // that separates an allowance from a reserve, this is what says so.
    expect(codexAccountUsageView(captured, justBeforeReset).diagnostics).toEqual([]);
  });
});

describe("codexAccountUsageView diagnostics are time-independent", () => {
  it("reports the same diagnostics at two far-apart instants", () => {
    // The invariant that makes it safe to run this reader twice per read — once
    // to log, once to render. The payload must actually *produce* diagnostics or
    // this compares two empty arrays and proves nothing.
    const { normalModelSlug: _dropped, ...unclassifiable } = ACCOUNT_BUCKET;
    const payload = {
      rateLimitsByLimitId: {
        unclassifiable,
        bad: { ...ACCOUNT_BUCKET, primary: { usedPercent: "nope", resetsAt: future(86_400) } },
      },
    };
    const early = codexAccountUsageView(payload, 0).diagnostics;
    const late = codexAccountUsageView(payload, future(10 * 365 * 86_400) * 1000).diagnostics;
    expect(early.length).toBeGreaterThan(0);
    expect(late).toEqual(early);
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
  ])("returns no windows for %s", (_case, payload) => {
    expect(codexAccountUsageView(payload, NOW).windows).toEqual([]);
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
