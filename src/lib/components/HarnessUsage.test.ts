import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, screen, fireEvent, waitFor, within } from "@testing-library/svelte";
import { tick } from "svelte";
import HarnessUsage from "./HarnessUsage.svelte";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => undefined),
}));

const usage = await import("$lib/state/harnessUsage.svelte");

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
    await renderCodexWithRateLimit({
      rateLimitType: "five_hour",
      resetsAt: epochFromNow(4 * 3600),
      isUsingOverage: true,
      overageResetsAt: epochFromNow(6 * 86400),
    });
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

async function renderCodexWithRateLimit(info: unknown, refused = false): Promise<void> {
  usage.observeUsage("codex", { payload: info, observed_at: new Date().toISOString() });
  if (refused) usage.recordUsageRefusal("codex");
  render(HarnessUsage);
  await tick();
}

/// Codex rate-limit windows — both independent windows (primary ~5-hour +
/// secondary weekly) surfaced as gauge lines, each labeled from its
/// `window_minutes` and gated reset-passed. The reset times (incl. the weekly
/// window, days out) live in the tooltip. Class B (session-file-backed), so no
/// snapshot-age line. Closes G8 (secondary window + reset times were dropped).
describe("Codex rate-limit windows", () => {
  it("renders both windows as meters carrying the harness-shared labels", async () => {
    await renderCodexWithRateLimit({
      primary: { used_percent: 42.0, window_minutes: 300, resets_at: epochFromNow(2 * 3600) },
      secondary: { used_percent: 7.0, window_minutes: 10080, resets_at: epochFromNow(5 * 86400) },
    });
    const meters = screen.getAllByTestId("harness-usage-window");
    expect(meters).toHaveLength(2);
    // window_minutes → the same strings the Claude cell uses, not
    // "primary/secondary" and not a Codex-only vocabulary.
    expect(meters[0]).toHaveTextContent("5-hour limit");
    expect(meters[0]).toHaveTextContent("42%");
    expect(meters[1]).toHaveTextContent("Weekly · all models");
    expect(meters[1]).toHaveTextContent("7%");
  });

  it("renders a bare used_percent as a 'Quota' meter", async () => {
    // A minimal payload — no duration to name the window — still reads as a
    // real gauge rather than disappearing.
    await renderCodexWithRateLimit({ primary: { used_percent: 42.5 } });
    const meter = screen.getByTestId("harness-usage-window");
    expect(meter).toHaveTextContent("Quota");
    expect(meter).toHaveTextContent("43%");
  });

  it("converts Codex's 0-100 percentage to the meter's used fraction", async () => {
    // The conversion happens at the derivation boundary so the meter only ever
    // sees a 0-1 fraction; a missed division would fill the bar at 4200%.
    await renderCodexWithRateLimit({
      primary: { used_percent: 42.0, window_minutes: 300, resets_at: epochFromNow(2 * 3600) },
    });
    expect(screen.getByTestId("harness-usage-window-fill")).toHaveStyle({ width: "42.0%" });
  });

  it("hides a window whose reset has passed (reset-passed), keeps the live one", async () => {
    await renderCodexWithRateLimit({
      primary: { used_percent: 42.0, window_minutes: 300, resets_at: epochFromNow(-3600) },
      secondary: { used_percent: 7.0, window_minutes: 10080, resets_at: epochFromNow(5 * 86400) },
    });
    const meters = screen.getAllByTestId("harness-usage-window");
    expect(meters).toHaveLength(1);
    expect(meters[0]).toHaveTextContent("Weekly · all models");
    expect(meters[0]).toHaveTextContent("7%");
  });

  it("surfaces reset times in the tooltip, not the inline gauge", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    try {
      await renderCodexWithRateLimit({
        primary: { used_percent: 42.0, window_minutes: 300, resets_at: epochFromNow(2 * 3600) },
        secondary: { used_percent: 7.0, window_minutes: 10080, resets_at: epochFromNow(5 * 86400) },
      });
      await fireEvent.pointerEnter(screen.getByTestId("harness-usage-codex"));
      await vi.advanceTimersByTimeAsync(500);
      const detail = await waitFor(() => screen.getByTestId("harness-usage-detail-codex"));
      expect(detail).toHaveTextContent(/5-hour limit\s+42% used\s+Resets/);
      expect(detail).toHaveTextContent(/Weekly · all models\s+7% used\s+Resets/);
    } finally {
      vi.useRealTimers();
    }
  });

  it("draws the window full and amber after Codex refused the agent's last turn", async () => {
    // The last measurement said 93%; the refusal says the window is used up.
    // The bar shows the verdict, since that is the number the user just hit.
    await renderCodexWithRateLimit(
      { primary: { used_percent: 93.0, window_minutes: 10080, resets_at: epochFromNow(86_400) } },
      true,
    );
    const meter = screen.getByTestId("harness-usage-window");
    expect(meter).toHaveTextContent("Weekly · all models");
    expect(meter).toHaveTextContent("100%");
    expect(screen.getByTestId("harness-usage-window-fill")).toHaveStyle({ width: "100.0%" });
    expect(screen.getByTestId("harness-usage-window-fill")).toHaveClass("bg-warning");
  });

  it("explains the full bar in the tooltip", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    try {
      await renderCodexWithRateLimit(
        { primary: { used_percent: 93.0, window_minutes: 10080, resets_at: epochFromNow(86_400) } },
        true,
      );
      await fireEvent.pointerEnter(screen.getByTestId("harness-usage-codex"));
      await vi.advanceTimersByTimeAsync(500);
      const detail = await waitFor(() => screen.getByTestId("harness-usage-detail-codex"));
      expect(detail).toHaveTextContent("100% used");
      // No sentence explaining the refusal: a full bar in the warning tone is
      // the statement, and spelling it out underneath was over-explaining.
      expect(screen.queryByTestId("harness-usage-refused")).toBeNull();
    } finally {
      vi.useRealTimers();
    }
  });

  it("leaves the measurement as it was when no refusal is standing", async () => {
    await renderCodexWithRateLimit(
      { primary: { used_percent: 93.0, window_minutes: 10080, resets_at: epochFromNow(86_400) } },
      false,
    );
    expect(screen.getByTestId("harness-usage-window")).toHaveTextContent("93%");
    expect(screen.getByTestId("harness-usage-window-fill")).not.toHaveClass("bg-warning");
  });

  it("marks only the most-used window, not every window the plan reports", async () => {
    // Codex says *a* limit was hit, never which. Flagging both would tell a
    // user who burned a 5-hour quota that their weekly one is gone too —
    // days claimed for an hour.
    await renderCodexWithRateLimit(
      {
        primary: { used_percent: 99.0, window_minutes: 300, resets_at: epochFromNow(1800) },
        secondary: { used_percent: 30.0, window_minutes: 10080, resets_at: epochFromNow(86_400) },
      },
      true,
    );
    const meters = screen.getAllByTestId("harness-usage-window");
    expect(meters).toHaveLength(2);
    expect(meters[0]).toHaveTextContent("5-hour limit");
    expect(meters[0]).toHaveTextContent("100%");
    expect(meters[1]).toHaveTextContent("Weekly · all models");
    expect(meters[1]).toHaveTextContent("30%");
    const fills = screen.getAllByTestId("harness-usage-window-fill");
    expect(fills[0]).toHaveClass("bg-warning");
    expect(fills[1]).not.toHaveClass("bg-warning");
  });

  it("states the refusal inline, without waiting for a hover", async () => {
    // This replaces a collapsed-card warning line that existed because the
    // meters used to live on a card that could be collapsed. The section has no
    // collapsed state, so the claim it protected — a refusal is legible without
    // hovering — now belongs to the meter itself: full bar, warning tone, and the
    // percentage in the row.
    await renderCodexWithRateLimit(
      { primary: { used_percent: 93.0, window_minutes: 10080, resets_at: epochFromNow(86_400) } },
      true,
    );
    const meter = screen.getByTestId("harness-usage-window");
    expect(meter).toHaveTextContent("Weekly · all models");
    expect(meter).toHaveTextContent("100%");
    expect(screen.getByTestId("harness-usage-window-fill")).toHaveClass("bg-warning");
  });

  it("Claude agent never shows the Codex gauge cell (Codex-gated)", async () => {
    await renderClaudeWithRateLimit(
      { primary: { used_percent: 42.0, window_minutes: 300, resets_at: epochFromNow(2 * 3600) } },
      null,
    );
    // Claude reads its own shape (isUsingOverage/resetsAt), not Codex's
    // primary.used_percent — so the Codex gauge cell must not appear.
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
      payload: {
        primary: { used_percent: 12, window_minutes: 10080, resets_at: epochFromNow(86_400) },
      },
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
      payload: {
        primary: { used_percent: 99, window_minutes: 10080, resets_at: epochFromNow(-60) },
      },
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
