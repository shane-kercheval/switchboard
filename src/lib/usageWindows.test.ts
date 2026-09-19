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
  rateLimitReachedType: null,
};

const RESERVE_BUCKET = {
  limitId: "base_model_inference",
  limitName: "gpt-reserve",
  normalModelSlug: "gpt-5.6-luna",
  primary: window(5, 10080),
  secondary: null,
  rateLimitReachedType: null,
};

const view = (
  buckets: Record<string, unknown>,
  allowed = true,
): ReturnType<typeof codexAccountUsageView> =>
  codexAccountUsageView(accountPayload(buckets, allowed), NOW);

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

describe("codexAccountUsageView exhaustion", () => {
  const capped = {
    ...ACCOUNT_BUCKET,
    primary: window(100, 10080),
    rateLimitReachedType: "rate_limit_reached",
  };

  it("marks a spent quota because Codex says so, without inflating the number", () => {
    // The measurement stands as measured. The reader this replaced painted its
    // guessed window to 100%, because the per-turn payload recorded no number
    // on a refusal.
    const { windows } = view({ codex: { ...capped, primary: window(97, 10080) } }, false);
    expect(windows[0]?.limitReached).toBe(true);
    expect(windows[0]?.usedFraction).toBeCloseTo(0.97);
  });

  it("marks a spent quota even at zero percent used", () => {
    // The rule this replaced vetoed the flag whenever usage read 0%, on the
    // reasoning that a quota cannot be both unspent and exhausted. That holds
    // only for consumption refusals.
    const { windows } = view({ codex: { ...capped, primary: window(0, 10080) } }, false);
    expect(windows[0]?.limitReached).toBe(true);
  });

  it("never marks a bar for a workspace-level restriction", () => {
    // Four of Codex's five restriction kinds are workspace facts that say
    // nothing about this window's consumption. Marking the bar would tell a
    // user whose workspace ran out of credits that their weekly quota is
    // exhausted, on a bar reading 42%.
    for (const reason of [
      "workspace_owner_credits_depleted",
      "workspace_member_credits_depleted",
      "workspace_owner_usage_limit_reached",
      "workspace_member_usage_limit_reached",
    ]) {
      // `ordinaryUsageAllowed` is left unanswered on purpose: the restriction
      // itself has to reach the account statement, or it is lost whenever the
      // backend declines to answer that field — which it is permitted to do.
      const payload = {
        rateLimitsByLimitId: { codex: { ...ACCOUNT_BUCKET, rateLimitReachedType: reason } },
      };
      const { windows, blocked } = codexAccountUsageView(payload, NOW);
      expect(windows[0]?.limitReached).toBeUndefined();
      expect(blocked).toBe(true);
    }
  });

  it("marks neither window when a two-window bucket reports a refusal", () => {
    // The protocol does not say which of a bucket's windows refused, and
    // picking one is the guess this reader deleted. The account-level statement
    // is what tells the user they are blocked.
    // Again with no account-level answer, so the bucket's own refusal is what
    // must surface. A two-window bucket names neither window, and picking one is
    // the guess this reader deleted.
    const payload = {
      rateLimitsByLimitId: {
        codex: { ...capped, primary: window(30, 300, 1800), secondary: window(100, 10080) },
      },
    };
    const { windows, blocked } = codexAccountUsageView(payload, NOW);
    expect(windows.map((w) => w.limitReached)).toEqual([undefined, undefined]);
    expect(blocked).toBe(true);
  });

  it("does not attribute a refusal to the survivor of an expired sibling", () => {
    // The bucket declared two windows, so the refusal is unattributable even
    // though only one window renders. Counting rendered windows instead of
    // declared ones would put the flag on whichever one happened to survive.
    const { windows } = view(
      { codex: { ...capped, primary: window(100, 300, -60), secondary: window(40, 10080) } },
      false,
    );
    expect(windows.map((w) => w.key)).toEqual(["codex:secondary"]);
    expect(windows[0]?.limitReached).toBeUndefined();
  });

  it("reads a recovered account as healthy", () => {
    const { windows, blocked } = view({ codex: ACCOUNT_BUCKET });
    expect(windows[0]?.limitReached).toBeUndefined();
    expect(blocked).toBe(false);
  });
});

describe("codexAccountUsageView account-level restriction", () => {
  it.each([
    ["an explicit false", false, true],
    ["an explicit true", true, false],
  ])("reports %s as blocked=%s", (_case, allowed, expected) => {
    expect(view({ codex: ACCOUNT_BUCKET }, allowed as boolean).blocked).toBe(expected);
  });

  it("does not state a restriction when an attributed bar already carries it", () => {
    // A single-window bucket's refusal lands on its bar, so the line would be
    // saying the same thing twice.
    const payload = {
      rateLimitsByLimitId: {
        codex: {
          ...ACCOUNT_BUCKET,
          primary: window(100, 10080),
          rateLimitReachedType: "rate_limit_reached",
        },
      },
    };
    const { windows, blocked } = codexAccountUsageView(payload, NOW);
    expect(windows[0]?.limitReached).toBe(true);
    expect(blocked).toBe(false);
  });

  it("states a restriction reported by a bucket whose windows all expired", () => {
    // Nothing renders, so the bucket's own refusal is the only thing left to
    // say — and it would be lost if the statement depended on the account gate.
    const payload = {
      rateLimitsByLimitId: {
        codex: {
          ...ACCOUNT_BUCKET,
          primary: window(100, 10080, -60),
          rateLimitReachedType: "rate_limit_reached",
        },
      },
    };
    const { windows, blocked } = codexAccountUsageView(payload, NOW);
    expect(windows).toEqual([]);
    expect(blocked).toBe(true);
  });

  it("treats an absent answer as absence, never as permission", () => {
    // The schema is emphatic that clients must not infer recovery; `null` means
    // the backend did not say.
    const payload = { rateLimitsByLimitId: { codex: ACCOUNT_BUCKET } };
    expect(codexAccountUsageView(payload, NOW).blocked).toBe(false);
  });

  it("reports a restriction even when no window survives", () => {
    // The shapes that produce a restriction most often are the ones that leave
    // nothing to draw. Requiring a meter would hide the harness's own answer in
    // the cases it matters most.
    const expired = { ...ACCOUNT_BUCKET, primary: window(100, 10080, -60) };
    const { windows, blocked } = view({ codex: expired }, false);
    expect(windows).toEqual([]);
    expect(blocked).toBe(true);
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

/// The account statement's sourcing, as a decision table rather than as a list
/// of examples. Both defects in this area were empty cells in a grid like this
/// one: a restriction reported on a bucket the meters skip, and a bucket-level
/// signal overriding an explicit answer. Enumerating the combinations is what
/// reaches interactions that breaking one condition at a time does not.
describe("codexAccountUsageView restriction routing", () => {
  const WORKSPACE = "workspace_owner_credits_depleted";
  const CONSUMPTION = "rate_limit_reached";

  /// Deliberately omits `ordinaryUsageAllowed` so the gate is unanswered and the
  /// bucket's own restriction is the only thing that can set the statement. A
  /// fixture that also set the gate would satisfy the assertion by the gate,
  /// which is precisely how the dropped-restriction defect passed its test.
  const gateSilent = (buckets: Record<string, unknown>): unknown => ({
    rateLimitsByLimitId: buckets,
  });

  const reserve = (reason: unknown): unknown => ({
    ...RESERVE_BUCKET,
    rateLimitReachedType: reason,
  });
  const account = (reason: unknown): unknown => ({
    ...ACCOUNT_BUCKET,
    rateLimitReachedType: reason,
  });

  it.each([
    ["a workspace restriction on the account quota", account(WORKSPACE), true],
    ["a workspace restriction on the reserve", reserve(WORKSPACE), true],
    ["a consumption refusal on the reserve", reserve(CONSUMPTION), false],
    ["no restriction at all", account(null), false],
  ])("with the gate unanswered, %s sets the statement: %s", (_case, bucket, expected) => {
    expect(codexAccountUsageView(gateSilent({ b: bucket }), NOW).blocked).toBe(expected);
  });

  it("routes a workspace restriction from a bucket it cannot even classify", () => {
    // No `normalModelSlug` key, so the bucket is skipped and reported — but a
    // workspace fact is not a property of the quota that carried it.
    const { normalModelSlug: _dropped, ...unclassifiable } = ACCOUNT_BUCKET;
    const view = codexAccountUsageView(
      gateSilent({ codex: { ...unclassifiable, rateLimitReachedType: WORKSPACE } }),
      NOW,
    );
    expect(view.blocked).toBe(true);
    expect(view.diagnostics.map((d) => d.kind)).toContain("bucket-without-model-association");
  });

  it("treats an unrecognised restriction kind as unattributable rather than absent", () => {
    // An additive enum on an experimental protocol: a sixth value must still
    // reach the statement rather than marking a bar or vanishing.
    const view = codexAccountUsageView(gateSilent({ codex: account("something_new") }), NOW);
    expect(view.blocked).toBe(true);
    expect(view.windows[0]?.limitReached).toBeUndefined();
  });

  it("drops an unrecognised restriction kind reported only on the reserve", () => {
    // Saying something about the account on an unknown reason, reported against
    // a quota we do not render, would assert more than we know.
    expect(codexAccountUsageView(gateSilent({ r: reserve("something_new") }), NOW).blocked).toBe(
      false,
    );
  });
});

/// The claim the comments make, asserted as a property over every combination
/// rather than trusted as prose beside code that cannot enforce it.
///
/// Three defects in this area were all a sweeping comment covering a branch the
/// code did not reach. A cross-product test fails on the first such branch and
/// keeps failing for the next one, which is what prose cannot do.
describe("no reported restriction disappears without a recorded reason", () => {
  const REASONS = [
    "rate_limit_reached",
    "workspace_owner_credits_depleted",
    "workspace_member_credits_depleted",
    "workspace_owner_usage_limit_reached",
    "workspace_member_usage_limit_reached",
    "an_unrecognised_future_kind",
  ];

  const SCOPES = [
    { name: "account-wide, one window", bucket: { ...ACCOUNT_BUCKET } },
    {
      name: "account-wide, two windows",
      bucket: { ...ACCOUNT_BUCKET, primary: window(30, 300, 1800), secondary: window(90, 10080) },
    },
    {
      name: "account-wide, window expired",
      bucket: { ...ACCOUNT_BUCKET, primary: window(100, 10080, -60) },
    },
    { name: "model-scoped reserve", bucket: { ...RESERVE_BUCKET } },
  ];

  /// The one combination that is *supposed* to produce nothing: a reserve
  /// reporting its own consumption is spent, which says nothing about ordinary
  /// work. An unrecognised kind on a reserve is silent for the same reason —
  /// asserting an account-level restriction from an unknown reason on a quota we
  /// do not render would claim more than we know.
  const deliberatelySilent = (scope: string, reason: string): boolean =>
    scope === "model-scoped reserve" && !reason.startsWith("workspace_");

  for (const scope of SCOPES) {
    for (const reason of REASONS) {
      it(`${scope.name} + ${reason}`, () => {
        // Gate left unanswered so the bucket's restriction is the only source.
        const view = codexAccountUsageView(
          { rateLimitsByLimitId: { b: { ...scope.bucket, rateLimitReachedType: reason } } },
          NOW,
        );
        const markedBar = view.windows.some((w) => w.limitReached === true);
        const surfaced = markedBar || view.blocked;
        if (deliberatelySilent(scope.name, reason)) {
          expect(surfaced).toBe(false);
        } else {
          expect(surfaced).toBe(true);
        }
        // Never both: a bar carrying it makes the account line a duplicate.
        expect(markedBar && view.blocked).toBe(false);
      });
    }
  }
});

describe("codexAccountUsageView gate precedence", () => {
  const capped = {
    ...ACCOUNT_BUCKET,
    primary: window(30, 300, 1800),
    secondary: window(100, 10080),
    rateLimitReachedType: "rate_limit_reached",
  };

  it("lets an explicit permission override an unattributable restriction", () => {
    // The schema calls this field the backend's validated permission for
    // ordinary usage, so when it speaks it is the authority. Without this the app
    // tells a user they are blocked immediately after Codex said they are not.
    const view = codexAccountUsageView(
      { ordinaryUsageAllowed: true, rateLimitsByLimitId: { codex: capped } },
      NOW,
    );
    expect(view.blocked).toBe(false);
  });

  it("reports the contradiction rather than resolving it in silence", () => {
    const view = codexAccountUsageView(
      { ordinaryUsageAllowed: true, rateLimitsByLimitId: { codex: capped } },
      NOW,
    );
    expect(view.diagnostics).toEqual([
      { kind: "restriction-despite-permission", limitId: "codex" },
    ]);
  });

  it("states the restriction on an explicit denial even with no bucket signal", () => {
    const view = codexAccountUsageView(
      { ordinaryUsageAllowed: false, rateLimitsByLimitId: { codex: ACCOUNT_BUCKET } },
      NOW,
    );
    expect(view.blocked).toBe(true);
    expect(view.diagnostics).toEqual([]);
  });

  it("leaves a bar marked from its own measurement even when permission is granted", () => {
    // A window-scoped mark is not an account-level claim, so it does not defer
    // to the account-level answer.
    const single = {
      ...ACCOUNT_BUCKET,
      primary: window(100, 10080),
      rateLimitReachedType: "rate_limit_reached",
    };
    const view = codexAccountUsageView(
      { ordinaryUsageAllowed: true, rateLimitsByLimitId: { codex: single } },
      NOW,
    );
    expect(view.windows[0]?.limitReached).toBe(true);
    expect(view.blocked).toBe(false);
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
