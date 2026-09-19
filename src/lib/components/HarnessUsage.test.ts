import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, screen, fireEvent, waitFor, within } from "@testing-library/svelte";
import { tick } from "svelte";
import HarnessUsage from "./HarnessUsage.svelte";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => undefined),
}));

const usage = await import("$lib/state/harnessUsage.svelte");
const accountUsage = await import("$lib/state/accountUsage.svelte");
const { invoke } = await import("@tauri-apps/api/core");

/// Rendering claims for the account-scoped usage section: meter counts, labels,
/// order, tone, the tooltip's absolute dates, and every clean-hide rule. What the
/// *derivation* accepts is covered in `usageWindows.test.ts`; this file is about
/// what a reader can actually see.
///
/// Every test here drives one store entry per harness rather than an agent
/// runtime. That is the relocation these tests exist to pin: the reading belongs
/// to the harness account, so no agent, no project, and no roster participates in
/// rendering it.
beforeEach(() => {
  usage._testing.reset();
  // Mounting fires a refresh, so a read left in flight by an earlier test would
  // make the next mount skip its own — the coalescing working as designed, and
  // a source of order-dependent tests if not cleared.
  accountUsage._testing.reset();
  vi.mocked(invoke).mockClear();
});

/// A unix-epoch-seconds timestamp `deltaSeconds` from now — payloads use
/// real-now-relative resets so the "is this window still in the future?" gate
/// is exercised deterministically (a fixed epoch would drift past `now` and
/// flip the test's meaning over time).
function epochFromNow(deltaSeconds: number): number {
  return Math.floor(Date.now() / 1000) + deltaSeconds;
}

/// An ISO string `ms` before now — for the snapshot-age (`as_of`) tooltip line.
function agoIso(ms: number): string {
  return new Date(Date.now() - ms).toISOString();
}

/// Seed the account-scoped store and render. **No agent and no project**: that
/// is the point of the relocation — a reading is a fact about the harness
/// account, so nothing here needs a roster to render it.
async function renderClaudeWithRateLimit(
  info: unknown,
  measuredAt: string | null,
  model?: string,
): Promise<void> {
  usage.observeUsage("claude_code", {
    payload: info,
    observed_at: measuredAt ?? new Date().toISOString(),
    model,
  });
  render(HarnessUsage);
  await tick();
}

/// The window keys the probe saw on a Fable turn, each with a used fraction and
/// a future reset. The offsets carry deliberate slack from the hour and day
/// boundaries: the countdown rounds up, so keeping each instant just below its
/// displayed boundary prevents clock jitter from changing the assertion.
function unifiedWindows(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    five_hour: { utilization: 0.33, resetsAt: epochFromNow(4 * 3600 - 60) },
    seven_day: { utilization: 0.27, resetsAt: epochFromNow(5 * 86400 - 3600) },
    ...overrides,
  };
}

/// Claude rate-limit surface with **no `unifiedWindows`** — an older CLI, or a
/// future one that drops the undocumented field. Every payload here exercises
/// the fallback (a bare reset line, no percentage, because there is no
/// percentage to show) plus the overage escalation, which is a separate signal
/// orthogonal to which window shape arrived. Each is gated on its own reset
/// being in the future (reset-passed → clean-hide). Exact clock/date text isn't
/// asserted (jsdom locale/timezone dependent) — only the stable label/copy and
/// presence/absence per the gating rules.
describe("Claude rate-limit fallback (no unifiedWindows)", () => {
  it("shows the fallback window independent of overage (normal-quota turn)", async () => {
    // No isUsingOverage — the window must still surface (the bug this fixed:
    // the window used to be gated on overage).
    await renderClaudeWithRateLimit(
      { status: "allowed", rateLimitType: "five_hour", resetsAt: epochFromNow(4 * 3600) },
      null,
    );
    const window = screen.getByTestId("harness-usage-fallback");
    expect(window).toHaveTextContent("5-hour limit resets");
    // Not overaging → no amber escalation.
    expect(screen.queryByTestId("harness-usage-overage")).toBeNull();
  });

  it("derives the fallback label from rateLimitType (unknown → generic)", async () => {
    await renderClaudeWithRateLimit(
      { status: "allowed", rateLimitType: "weekly", resetsAt: epochFromNow(4 * 3600) },
      null,
    );
    // Unknown type falls back to the generic label, never a hardcoded "5-hour".
    const window = screen.getByTestId("harness-usage-fallback");
    expect(window).toHaveTextContent("rate limit resets");
    expect(window).not.toHaveTextContent("5-hour");
  });

  it("hides the fallback window once its reset is in the past (reset-passed)", async () => {
    // A past reset is known-stale (the window has cycled, we lack the new
    // reset) — showing a past 'resets at' would be wrong, so it clean-hides.
    await renderClaudeWithRateLimit(
      { status: "allowed", rateLimitType: "five_hour", resetsAt: epochFromNow(-3600) },
      null,
    );
    expect(screen.queryByTestId("harness-usage-fallback")).toBeNull();
    expect(screen.queryByTestId("harness-usage-claude_code")).toBeNull();
  });

  it("shows the amber overage escalation when overaging with a future overage window", async () => {
    await renderClaudeWithRateLimit(
      {
        status: "rejected",
        rateLimitType: "five_hour",
        resetsAt: epochFromNow(4 * 3600),
        isUsingOverage: true,
        overageResetsAt: epochFromNow(6 * 86400),
      },
      null,
    );
    // Both signals present: neutral fallback line + amber escalation.
    expect(screen.getByTestId("harness-usage-fallback")).toHaveTextContent("5-hour limit resets");
    const overage = screen.getByTestId("harness-usage-overage");
    expect(overage).toHaveTextContent("using credits");
    expect(overage).toHaveClass("text-warning");
  });

  it("drops the overage escalation once the overage window has passed", async () => {
    // isUsingOverage true, but the overage window elapsed → the credit window
    // has cycled, so the escalation is stale and hidden. The still-future
    // fallback window stays.
    await renderClaudeWithRateLimit(
      {
        status: "rejected",
        rateLimitType: "five_hour",
        resetsAt: epochFromNow(4 * 3600),
        isUsingOverage: true,
        overageResetsAt: epochFromNow(-3600),
      },
      null,
    );
    expect(screen.getByTestId("harness-usage-fallback")).toBeInTheDocument();
    expect(screen.queryByTestId("harness-usage-overage")).toBeNull();
  });

  it("overage flag with no overage window still shows (can't prove it stale)", async () => {
    await renderClaudeWithRateLimit(
      { isUsingOverage: true, resetsAt: epochFromNow(4 * 3600), rateLimitType: "five_hour" },
      null,
    );
    expect(screen.getByTestId("harness-usage-overage")).toHaveTextContent("using credits");
  });

  it("renders nothing when there is no usable rate-limit signal", async () => {
    // Everything elapsed / absent → the whole cell clean-hides.
    await renderClaudeWithRateLimit({ status: "allowed", resetsAt: epochFromNow(-3600) }, null);
    expect(screen.queryByTestId("harness-usage-claude_code")).toBeNull();
    expect(screen.queryByTestId("harness-usage-fallback")).toBeNull();
    expect(screen.queryByTestId("harness-usage-overage")).toBeNull();
  });

  it("shows no meter and no percentage on the fallback path", async () => {
    // The point of the fallback: the top-level pair carries a reset but no
    // utilization, and a bar drawn without a value would read as 0% used.
    await renderClaudeWithRateLimit(
      { status: "allowed", rateLimitType: "five_hour", resetsAt: epochFromNow(4 * 3600) },
      null,
    );
    expect(screen.queryAllByTestId("harness-usage-window")).toHaveLength(0);
    expect(screen.getByTestId("harness-usage-claude_code")).not.toHaveTextContent("%");
  });

  it("never reads a Claude-shaped payload stored under Codex", async () => {
    // Each entry is interpreted only by its own harness's reader, so a
    // Claude-shaped payload filed under Codex yields nothing rather than
    // Claude's cells. The gate moved from "which agent card is this" to "which
    // entry is this", and it still has to hold.
    usage.observeUsage("codex", {
      payload: {
        rateLimitType: "five_hour",
        resetsAt: epochFromNow(4 * 3600),
        isUsingOverage: true,
        overageResetsAt: epochFromNow(6 * 86400),
      },
      observed_at: new Date().toISOString(),
    });
    render(HarnessUsage);
    await tick();
    expect(screen.queryByTestId("harness-usage-fallback")).toBeNull();
    expect(screen.queryByTestId("harness-usage-overage")).toBeNull();
    expect(screen.queryByTestId("harness-usage-codex")).toBeNull();
  });
});

/// Claude usage windows — the primary path. `unifiedWindows` carries every
/// window the desktop app shows, each with a 0-1 used fraction, and is
/// authoritative whenever present.
describe("Claude usage windows", () => {
  it("renders one meter per window, with the fraction as a percentage", async () => {
    await renderClaudeWithRateLimit({ status: "allowed", unifiedWindows: unifiedWindows() }, null);

    const meters = screen.getAllByTestId("harness-usage-window");
    expect(meters).toHaveLength(2);
    expect(meters[0]).toHaveTextContent("5-hour limit");
    expect(meters[0]).toHaveTextContent("33%");
    expect(meters[1]).toHaveTextContent("Weekly · all models");
    expect(meters[1]).toHaveTextContent("27%");
  });

  it("shows each window's countdown to its own reset", async () => {
    await renderClaudeWithRateLimit({ status: "allowed", unifiedWindows: unifiedWindows() }, null);

    const meters = screen.getAllByTestId("harness-usage-window");
    // Two windows resetting at different distances must not share one
    // countdown; the 5-hour one is hours out and the weekly one days.
    expect(meters[0]).toHaveTextContent("in 4 h");
    expect(meters[1]).toHaveTextContent("in 5 d");
  });

  it("ignores the top-level fallback pair when unifiedWindows is present", async () => {
    // `unifiedWindows` is authoritative, so the bare reset line must not
    // double-render the same window beneath the meters.
    await renderClaudeWithRateLimit(
      {
        status: "allowed",
        rateLimitType: "five_hour",
        resetsAt: epochFromNow(4 * 3600),
        unifiedWindows: unifiedWindows(),
      },
      null,
    );
    expect(screen.getAllByTestId("harness-usage-window")).toHaveLength(2);
    expect(screen.queryByTestId("harness-usage-fallback")).toBeNull();
  });

  it("labels the per-model weekly window with the model that delivered it", async () => {
    await renderClaudeWithRateLimit(
      {
        status: "allowed",
        unifiedWindows: unifiedWindows({
          seven_day_overage_included: { utilization: 0.79, resetsAt: epochFromNow(5 * 86400) },
        }),
      },
      null,
      "claude-fable-5-1",
    );
    const meters = screen.getAllByTestId("harness-usage-window");
    expect(meters).toHaveLength(3);
    // Order is fixed by the key list, not by object iteration order.
    // The family name the user selected by, not the raw stream id — the label
    // shares a column with a countdown and a percentage.
    expect(meters[2]).toHaveTextContent("Weekly · Fable");
    expect(meters[2]).toHaveTextContent("79%");
  });

  it("falls back to a generic per-model label for a legacy snapshot without a model", async () => {
    await renderClaudeWithRateLimit(
      {
        status: "allowed",
        unifiedWindows: unifiedWindows({
          seven_day_overage_included: { utilization: 0.79, resetsAt: epochFromNow(5 * 86400) },
        }),
      },
      null,
      undefined,
    );
    const meters = screen.getAllByTestId("harness-usage-window");
    expect(meters[2]).toHaveTextContent("Weekly · model-specific");
  });

  it("fills only the flagged window amber when the CLI reports a threshold", async () => {
    await renderClaudeWithRateLimit(
      {
        status: "allowed_warning",
        rateLimitType: "seven_day_overage_included",
        surpassedThreshold: 0.75,
        unifiedWindows: unifiedWindows({
          seven_day_overage_included: { utilization: 0.79, resetsAt: epochFromNow(5 * 86400) },
        }),
      },
      null,
      "claude-fable-5-1",
    );
    const fills = screen.getAllByTestId("harness-usage-window-fill");
    expect(fills).toHaveLength(3);
    // The tone comes from the harness naming that window, not from a
    // percentage we chose — the 5-hour window at 33% stays calm.
    expect(fills[0]).toHaveClass("bg-fg");
    expect(fills[1]).toHaveClass("bg-fg");
    expect(fills[2]).toHaveClass("bg-warning");
  });

  it("drops a window whose reset has passed and keeps its siblings", async () => {
    await renderClaudeWithRateLimit(
      {
        status: "allowed",
        unifiedWindows: unifiedWindows({
          five_hour: { utilization: 0.33, resetsAt: epochFromNow(-3600) },
        }),
      },
      null,
    );
    const meters = screen.getAllByTestId("harness-usage-window");
    expect(meters).toHaveLength(1);
    expect(meters[0]).toHaveTextContent("Weekly · all models");
  });

  it("drops a window key it does not recognize", async () => {
    // The CLI binary lists keys that are not Claude Code windows on any plan we
    // can probe. A junk label is worse than a dropped window.
    await renderClaudeWithRateLimit(
      {
        status: "allowed",
        unifiedWindows: unifiedWindows({
          seven_day_cowork: { utilization: 0.5, resetsAt: epochFromNow(5 * 86400) },
        }),
      },
      null,
    );
    expect(screen.getAllByTestId("harness-usage-window")).toHaveLength(2);
    expect(screen.getByTestId("harness-usage-claude_code")).not.toHaveTextContent("cowork");
  });

  // The input-validation matrix — malformed fractions, missing resets,
  // non-object entries — moved to `usageWindows.test.ts`, which tests the
  // derivation directly. A rendered card can only show those as an absent
  // meter; what stays here is what only a render can prove.

  it("treats an empty window container as absent and falls back", async () => {
    // Nothing reported means the top-level pair is still the best signal
    // available; a blank cell would withhold a reset time we have.
    await renderClaudeWithRateLimit(
      {
        status: "allowed",
        rateLimitType: "five_hour",
        resetsAt: epochFromNow(4 * 3600 + 60),
        unifiedWindows: {},
      },
      null,
    );
    expect(screen.queryAllByTestId("harness-usage-window")).toHaveLength(0);
    expect(screen.getByTestId("harness-usage-fallback")).toHaveTextContent("5-hour limit resets");
  });

  it("stays authoritative when a non-empty container's windows were all dropped", async () => {
    // The windows were filtered on purpose — this one's reset has passed — so
    // the container still spoke. Falling back to the top-level pair here would
    // override the per-window rule rather than fill a gap.
    await renderClaudeWithRateLimit(
      {
        status: "allowed",
        rateLimitType: "five_hour",
        resetsAt: epochFromNow(4 * 3600 + 60),
        unifiedWindows: { five_hour: { utilization: 0.33, resetsAt: epochFromNow(-3600) } },
      },
      null,
    );
    expect(screen.queryAllByTestId("harness-usage-window")).toHaveLength(0);
    expect(screen.queryByTestId("harness-usage-fallback")).toBeNull();
    expect(screen.queryByTestId("harness-usage-claude_code")).toBeNull();
  });

  it("keeps the overage escalation beneath the meters", async () => {
    await renderClaudeWithRateLimit(
      {
        status: "allowed",
        isUsingOverage: true,
        overageResetsAt: epochFromNow(6 * 86400),
        unifiedWindows: unifiedWindows(),
      },
      null,
    );
    expect(screen.getAllByTestId("harness-usage-window")).toHaveLength(2);
    const overage = screen.getByTestId("harness-usage-overage");
    expect(overage).toHaveTextContent("using credits");
    expect(overage).toHaveClass("text-warning");
  });
});

/// Rate-limit tooltip content — always present when the cell shows, carrying
/// full reset dates (a window can be days out, beyond the inline clock) plus
/// both windows and the snapshot age when rehydrated. Mirrors the
/// parse-warnings tooltip test's fake-timer + pointerEnter pattern.
describe("Claude rate-limit tooltip", () => {
  beforeEach(() => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("surfaces the window + overage windows on hover; no snapshot line when live", async () => {
    await renderClaudeWithRateLimit(
      {
        status: "rejected",
        rateLimitType: "five_hour",
        resetsAt: epochFromNow(4 * 3600),
        isUsingOverage: true,
        overageResetsAt: epochFromNow(6 * 86400),
      },
      null,
    );
    await fireEvent.pointerEnter(screen.getByTestId("harness-usage-claude_code"));
    await vi.advanceTimersByTimeAsync(500);
    const detail = await waitFor(() => screen.getByTestId("harness-usage-detail-claude_code"));
    expect(detail).toHaveTextContent("5-hour limit");
    expect(detail).toHaveTextContent("Resets");
    // The overage window is surfaced here.
    expect(detail).toHaveTextContent(/overage window resets/i);
    // Every reading dates itself, including a live one.
    expect(screen.getByTestId("harness-usage-measured")).toHaveTextContent(/measured/i);
  });

  it("spells out each window's percentage and full reset date on hover", async () => {
    // The inline countdown is compressed to fit the card column; the tooltip is
    // where the absolute date and the word "used" have room.
    await renderClaudeWithRateLimit({ status: "allowed", unifiedWindows: unifiedWindows() }, null);
    await fireEvent.pointerEnter(screen.getByTestId("harness-usage-claude_code"));
    await vi.advanceTimersByTimeAsync(500);
    const detail = await waitFor(() => screen.getByTestId("harness-usage-detail-claude_code"));
    expect(detail).toHaveTextContent(/5-hour limit\s+33% used\s+Resets/);
    expect(detail).toHaveTextContent(/Weekly · all models\s+27% used\s+Resets/);
  });

  it("does not expose the harness's internal warning threshold in the tooltip", async () => {
    await renderClaudeWithRateLimit(
      {
        status: "allowed_warning",
        rateLimitType: "seven_day",
        surpassedThreshold: 0.75,
        unifiedWindows: unifiedWindows({
          seven_day: { utilization: 0.79, resetsAt: epochFromNow(5 * 86400 + 3600) },
        }),
      },
      null,
    );
    await fireEvent.pointerEnter(screen.getByTestId("harness-usage-claude_code"));
    await vi.advanceTimersByTimeAsync(500);
    const detail = await waitFor(() => screen.getByTestId("harness-usage-detail-claude_code"));
    expect(within(detail).getByText("Weekly · all models")).toBeInTheDocument();
    expect(within(detail).queryByText("Warning threshold")).toBeNull();
    expect(within(detail).queryByText("75%")).toBeNull();
    expect(within(detail).getAllByText("Resets")).toHaveLength(2);
  });

  it("dates the reading and says how to refresh it", async () => {
    // The age line is unconditional and harness-agnostic, because an
    // account-level reading is only as fresh as the last turn any agent ran —
    // a durable session-file reading can itself be days old.
    await renderClaudeWithRateLimit(
      { status: "allowed", rateLimitType: "five_hour", resetsAt: epochFromNow(4 * 3600) },
      agoIso(3 * 60 * 60 * 1000),
    );
    await fireEvent.pointerEnter(screen.getByTestId("harness-usage-claude_code"));
    await vi.advanceTimersByTimeAsync(500);
    await waitFor(() => screen.getByTestId("harness-usage-detail-claude_code"));
    const measured = screen.getByTestId("harness-usage-measured");
    expect(measured).toHaveTextContent(/measured .* ago/i);
    expect(measured).toHaveTextContent(/refresh/i);
  });
});

/// One bucket of the account read's response. Defaults describe the healthy
/// account-wide shape every capture shows: **unnamed** (`limitName` is null on
/// the account allowance; only the model reserve carries a name) and carrying no
/// associated model, which is what marks it account-wide.
function codexBucket(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    limitId: "codex",
    limitName: null,
    normalModelSlug: null,
    primary: { usedPercent: 42.0, windowDurationMins: 10080, resetsAt: epochFromNow(86_400) },
    rateLimitReachedType: null,
    ...overrides,
  };
}

/// The account read's payload, wrapped the way it is stored.
function codexAccount(buckets: Record<string, unknown>): unknown {
  return { ordinaryUsageAllowed: true, rateLimitsByLimitId: buckets };
}

async function renderCodexAccountUsage(
  buckets: Record<string, unknown>,
  ordinaryUsageAllowed = true,
): Promise<void> {
  usage.observeUsage("codex", {
    payload: { ordinaryUsageAllowed, rateLimitsByLimitId: buckets },
    observed_at: new Date().toISOString(),
  });
  render(HarnessUsage);
  await tick();
}

/// Codex account quotas — every metered limit the account holds, each named by
/// Codex and stating its own exhaustion.
///
/// This replaced a cell fed by the per-turn rollout, which reported one bucket
/// under identical identifiers whichever limit it described. That cell had to
/// name a window from its duration and guess which window a refusal belonged
/// to; both guesses are gone, and the tests that pinned them went with the
/// behaviour.
describe("Codex account quotas", () => {
  it("renders an account-wide quota as a meter", async () => {
    await renderCodexAccountUsage({ codex: codexBucket() });
    const meters = screen.getAllByTestId("harness-usage-window");
    expect(meters).toHaveLength(1);
    expect(meters[0]).toHaveTextContent("42%");
  });

  it("names a quota from Codex's own field", async () => {
    await renderCodexAccountUsage({ codex: codexBucket({ limitName: "Weekly" }) });
    expect(screen.getByTestId("harness-usage-window")).toHaveTextContent("Weekly");
  });

  it("labels an unnamed account-wide quota in the harness-shared vocabulary", async () => {
    // The same words Claude's weekly row uses, so the two harnesses' rows can be
    // read side by side as quantities rather than as two vocabularies. The cell
    // this replaced showed the same string for the wrong reason: it could not
    // tell the account allowance from the model reserve, so "all models" was an
    // assertion. Here it is what passing the account-wide filter means.
    await renderCodexAccountUsage({ codex: codexBucket() });
    expect(screen.getByTestId("harness-usage-window")).toHaveTextContent("Weekly · all models");
  });

  it("hides a model-specific reserve and shows the account-wide quota beside it", async () => {
    // The live account reports exactly this pair. Showing the reserve invites
    // reading its 95% headroom as the allowance governing ordinary work.
    await renderCodexAccountUsage({
      codex: codexBucket(),
      base_model_inference: codexBucket({
        limitId: "base_model_inference",
        limitName: "gpt-reserve",
        normalModelSlug: "gpt-5.6-luna",
        primary: { usedPercent: 5, windowDurationMins: 10080, resetsAt: epochFromNow(86_400) },
      }),
    });
    const meters = screen.getAllByTestId("harness-usage-window");
    expect(meters).toHaveLength(1);
    expect(meters[0]).toHaveTextContent("42%");
    expect(screen.queryByText(/gpt-reserve/)).toBeNull();
  });

  it("renders two account-wide quotas when the plan carries both", async () => {
    await renderCodexAccountUsage({
      five_hour: codexBucket({
        limitId: "five_hour",
        primary: { usedPercent: 12, windowDurationMins: 300, resetsAt: epochFromNow(1800) },
      }),
      weekly: codexBucket({ limitId: "weekly" }),
    });
    // Distinguished by their own window durations, with no name from Codex on
    // either — which is the case a neutral "Quota" fallback would have rendered
    // as two identical rows.
    const meters = screen.getAllByTestId("harness-usage-window");
    expect(meters).toHaveLength(2);
    expect(meters[0]).toHaveTextContent("5-hour limit");
    expect(meters[1]).toHaveTextContent("Weekly · all models");
  });

  it("converts Codex's 0-100 percentage to the meter's used fraction", async () => {
    // A missed division would fill the bar at 4200%.
    await renderCodexAccountUsage({ codex: codexBucket() });
    expect(screen.getByTestId("harness-usage-window-fill")).toHaveStyle({ width: "42.0%" });
  });

  it("hides a quota whose reset has passed, keeps the live one", async () => {
    await renderCodexAccountUsage({
      stale: codexBucket({
        limitId: "stale",
        primary: { usedPercent: 99, windowDurationMins: 300, resetsAt: epochFromNow(-3600) },
      }),
      live: codexBucket({ limitId: "live" }),
    });
    const meters = screen.getAllByTestId("harness-usage-window");
    expect(meters).toHaveLength(1);
    expect(meters[0]).toHaveTextContent("Weekly · all models");
  });

  it("skips a windowless quota", async () => {
    // Observed on a refused turn: a bucket arrives with its windows null. A bar
    // with no value is worse than no bar.
    await renderCodexAccountUsage({ premium: codexBucket({ primary: null }) });
    expect(screen.queryByTestId("harness-usage")).toBeNull();
  });

  it("surfaces reset times in the tooltip, not the inline gauge", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    try {
      await renderCodexAccountUsage({ codex: codexBucket() });
      await fireEvent.pointerEnter(screen.getByTestId("harness-usage-codex"));
      await vi.advanceTimersByTimeAsync(500);
      const detail = await waitFor(() => screen.getByTestId("harness-usage-detail-codex"));
      expect(detail).toHaveTextContent(/Weekly · all models\s+42% used\s+Resets/);
    } finally {
      vi.useRealTimers();
    }
  });

  it("draws a quota amber when Codex reports it exhausted, without inflating the number", async () => {
    // The bar shows what Codex measured. The cell this replaced overwrote the
    // measurement with 100%, because the per-turn payload recorded no number on
    // a refusal and the stale one would have contradicted the refusal beside it.
    await renderCodexAccountUsage({
      codex: codexBucket({
        primary: { usedPercent: 97, windowDurationMins: 10080, resetsAt: epochFromNow(86_400) },
        rateLimitReachedType: "rate_limit_reached",
      }),
    });
    const meter = screen.getByTestId("harness-usage-window");
    expect(meter).toHaveTextContent("97%");
    expect(screen.getByTestId("harness-usage-window-fill")).toHaveClass("bg-warning");
  });

  it("flags only the quota that reports itself exhausted", async () => {
    // Each bucket carries its own verdict, so a spent 5-hour quota cannot
    // condemn the weekly one — days of waiting claimed for an hour of it, which
    // is what the old most-used guess risked on every refusal.
    await renderCodexAccountUsage({
      five_hour: codexBucket({
        limitId: "five_hour",
        primary: { usedPercent: 100, windowDurationMins: 300, resetsAt: epochFromNow(1800) },
        rateLimitReachedType: "rate_limit_reached",
      }),
      weekly: codexBucket({
        limitId: "weekly",
        primary: { usedPercent: 30, windowDurationMins: 10080, resetsAt: epochFromNow(86_400) },
      }),
    });
    const fills = screen.getAllByTestId("harness-usage-window-fill");
    expect(fills).toHaveLength(2);
    expect(fills[0]).toHaveClass("bg-warning");
    expect(fills[1]).not.toHaveClass("bg-warning");
  });

  it("leaves a healthy quota in the neutral tone", async () => {
    await renderCodexAccountUsage({ codex: codexBucket({ primary: { usedPercent: 93 } }) });
    expect(screen.getByTestId("harness-usage-window")).toHaveTextContent("93%");
    expect(screen.getByTestId("harness-usage-window-fill")).not.toHaveClass("bg-warning");
  });

  it("states exhaustion inline, without waiting for a hover", async () => {
    // The section has no collapsed state, so the claim a warning line used to
    // carry belongs to the meter itself: the percentage and the tone in the row.
    await renderCodexAccountUsage({
      codex: codexBucket({
        primary: { usedPercent: 100, resetsAt: epochFromNow(86_400) },
        rateLimitReachedType: "rate_limit_reached",
      }),
    });
    const meter = screen.getByTestId("harness-usage-window");
    expect(meter).toHaveTextContent("100%");
    expect(screen.getByTestId("harness-usage-window-fill")).toHaveClass("bg-warning");
  });

  it("renders nothing for a Codex entry still holding the old rollout shape", async () => {
    // An entry persisted before this cut carries `{primary, secondary}` at the
    // top level with no bucket map. It is structurally unreadable by this view,
    // which is the point: the old keys are not iterated as if they were buckets,
    // so nothing renders a quota named "primary". The next read supersedes it.
    usage.observeUsage("codex", {
      payload: {
        primary: { used_percent: 42.0, window_minutes: 300, resets_at: epochFromNow(2 * 3600) },
        secondary: { used_percent: 7.0, window_minutes: 10080, resets_at: epochFromNow(5 * 86400) },
      },
      observed_at: new Date().toISOString(),
    });
    render(HarnessUsage);
    await tick();
    expect(screen.queryByTestId("harness-usage-codex")).toBeNull();
    expect(screen.queryByText(/primary/i)).toBeNull();
  });

  it("renders a Claude entry beside an unreadable old-shape Codex one", async () => {
    // The loader is harness-agnostic, so an unreadable Codex entry must not
    // take the Claude row down with it.
    usage.observeUsage("codex", {
      payload: { primary: { used_percent: 42.0, resets_at: epochFromNow(3600) } },
      observed_at: new Date().toISOString(),
    });
    usage.observeUsage("claude_code", {
      payload: {
        status: "allowed",
        unifiedWindows: { five_hour: { utilization: 0.28, resetsAt: epochFromNow(3600) } },
      },
      observed_at: new Date().toISOString(),
    });
    render(HarnessUsage);
    await tick();
    expect(screen.queryByTestId("harness-usage-codex")).toBeNull();
    expect(screen.getByTestId("harness-usage-claude_code")).toBeInTheDocument();
  });

  it("renders both windows of a single bucket as separate meters", async () => {
    // A bucket holds up to two windows for the same limit. Rendering only the
    // first drops the second quota from the panel entirely.
    await renderCodexAccountUsage({
      codex: codexBucket({
        primary: { usedPercent: 12, windowDurationMins: 300, resetsAt: epochFromNow(1800) },
        secondary: { usedPercent: 70, windowDurationMins: 10080, resetsAt: epochFromNow(86_400) },
      }),
    });
    const meters = screen.getAllByTestId("harness-usage-window");
    expect(meters).toHaveLength(2);
    expect(meters[0]).toHaveTextContent("5-hour limit");
    expect(meters[1]).toHaveTextContent("Weekly · all models");
  });

  it("states an account-level restriction beside the meters", async () => {
    await renderCodexAccountUsage({ codex: codexBucket() }, false);
    expect(screen.getByTestId("harness-usage-blocked")).toBeInTheDocument();
    expect(screen.getByTestId("harness-usage-window")).toBeInTheDocument();
  });

  it("states an account-level restriction even when no meter survives", async () => {
    // The shapes that produce a restriction most often leave nothing to draw —
    // a workspace limit against windows that have all cycled. Hiding the row
    // for want of a meter would suppress the harness's own explicit answer at
    // the moment it matters most.
    await renderCodexAccountUsage(
      {
        codex: codexBucket({
          primary: { usedPercent: 100, windowDurationMins: 10080, resetsAt: epochFromNow(-60) },
        }),
      },
      false,
    );
    expect(screen.getByTestId("harness-usage-codex")).toBeInTheDocument();
    expect(screen.getByTestId("harness-usage-blocked")).toBeInTheDocument();
    expect(screen.queryByTestId("harness-usage-window")).toBeNull();
  });

  it("says nothing about restriction on a healthy account", async () => {
    await renderCodexAccountUsage({ codex: codexBucket() });
    expect(screen.queryByTestId("harness-usage-blocked")).toBeNull();
  });

  it("leaves every bar neutral when a workspace restriction is in force", async () => {
    // A workspace credit problem is not a statement about any window's
    // consumption, so no bar turns amber for it; the account line carries it.
    await renderCodexAccountUsage(
      { codex: codexBucket({ rateLimitReachedType: "workspace_owner_credits_depleted" }) },
      false,
    );
    expect(screen.getByTestId("harness-usage-window-fill")).not.toHaveClass("bg-warning");
    expect(screen.getByTestId("harness-usage-blocked")).toBeInTheDocument();
  });

  it("asks the account for a fresh reading when the panel mounts", async () => {
    // One of only three refresh triggers, and the only one that fires while the
    // app is already running without a turn ending. Deleting it would leave a
    // user who reopens the panel looking at whatever was last read, and until
    // this assertion existed nothing in the suite noticed its absence.
    render(HarnessUsage);
    await tick();
    expect(invoke).toHaveBeenCalledWith("read_codex_account_usage");
  });

  it("Claude agent never shows the Codex gauge cell (Codex-gated)", async () => {
    await renderClaudeWithRateLimit(codexAccount({ codex: codexBucket() }), null);
    // Claude reads its own shape, not the account bucket map — so the Codex
    // gauge cell must not appear.
    expect(screen.queryByTestId("harness-usage-codex")).toBeNull();
  });
});

/// What a fresh install shows, and what each partial state shows. These are the
/// states a new user sees first and the only ones nobody exercises by accident,
/// so they are pinned rather than eyeballed.
describe("HarnessUsage with nothing to report", () => {
  it("renders nothing at all before any harness has reported a reading", async () => {
    // Not an empty section with a header: the whole block is absent, so a fresh
    // install shows the agent roster with no space taken by a heading that has
    // nothing under it. Same clean-hide rule the card cells follow.
    render(HarnessUsage);
    await tick();
    expect(screen.queryByTestId("harness-usage")).toBeNull();
    expect(screen.queryByText(/usage limits/i)).toBeNull();
  });

  it("shows only the harnesses that have reported, not a row per installed harness", async () => {
    // A harness with no reading is absent rather than shown as empty or zero. A
    // zero meter would be a claim we cannot make: no reading is not 0% used.
    usage.observeUsage("codex", {
      payload: codexAccount({ codex: codexBucket({ primary: { usedPercent: 12 } }) }),
      observed_at: new Date().toISOString(),
    });
    render(HarnessUsage);
    await tick();
    expect(screen.getByTestId("harness-usage-codex")).toBeInTheDocument();
    expect(screen.queryByTestId("harness-usage-claude_code")).toBeNull();
    expect(screen.getAllByTestId("harness-usage-window")).toHaveLength(1);
  });

  it("drops a harness whose only window has already reset, back to rendering nothing", async () => {
    // A reading exists but says nothing current, which is the reset-passed rule
    // meeting the empty case: the section disappears rather than showing a
    // harness label with no meter under it.
    usage.observeUsage("codex", {
      payload: codexAccount({
        codex: codexBucket({ primary: { usedPercent: 99, resetsAt: epochFromNow(-60) } }),
      }),
      observed_at: new Date().toISOString(),
    });
    render(HarnessUsage);
    await tick();
    expect(screen.queryByTestId("harness-usage")).toBeNull();
  });

  it("renders nothing for a harness that reports no quota at all", async () => {
    // Antigravity emits no rate-limit signal by decision. An entry filed under it
    // must not produce a labelled row with an empty body.
    usage.observeUsage("antigravity", {
      payload: { primary: { used_percent: 50 } },
      observed_at: new Date().toISOString(),
    });
    render(HarnessUsage);
    await tick();
    expect(screen.queryByTestId("harness-usage")).toBeNull();
  });
});

describe("HarnessUsage for a reading with no measured instant", () => {
  it("renders the meters but claims no age", async () => {
    // A Codex record whose line carried no parseable timestamp produces this. The
    // reading is still worth showing; its age is not knowable, so nothing is
    // said about it rather than a fabricated date being rendered.
    usage.observeUsage("codex", {
      payload: codexAccount({ codex: codexBucket({ primary: { usedPercent: 41 } }) }),
    });
    render(HarnessUsage);
    await tick();
    expect(screen.getByTestId("harness-usage-window")).toHaveTextContent("41%");
    expect(screen.queryByTestId("harness-usage-measured")).toBeNull();
  });
});

describe("HarnessUsage percentage alignment", () => {
  function reservedWidths(): string[] {
    return Array.from(document.querySelectorAll<HTMLElement>("[style*='min-width']")).map(
      (el) => el.style.minWidth,
    );
  }

  it("widens the column for every row once any reading reaches three digits", async () => {
    // The rows read as one stacked list, so a full window on one harness has to
    // widen the column on the other or their detail text stops lining up.
    usage.observeUsage("codex", {
      payload: codexAccount({ codex: codexBucket({ primary: { usedPercent: 100 } }) }),
      observed_at: new Date().toISOString(),
    });
    usage.observeUsage("claude_code", {
      payload: {
        status: "allowed",
        unifiedWindows: { five_hour: { utilization: 0.28, resetsAt: epochFromNow(3600) } },
      },
      observed_at: new Date().toISOString(),
    });
    render(HarnessUsage);
    await tick();
    const widths = reservedWidths();
    expect(widths).toHaveLength(2);
    expect(new Set(widths)).toEqual(new Set(["3ch"]));
  });

  it("does not indent a section that never reaches three digits", async () => {
    usage.observeUsage("codex", {
      payload: codexAccount({ codex: codexBucket({ primary: { usedPercent: 93 } }) }),
      observed_at: new Date().toISOString(),
    });
    render(HarnessUsage);
    await tick();
    expect(reservedWidths()).toEqual(["2ch"]);
  });
});
