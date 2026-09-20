<script lang="ts">
  /// Account-scoped quota meters, one row per harness that has reported a
  /// reading.
  ///
  /// **One section pinned to the bottom of the sidebar rather than a cell on
  /// each agent card, and fed from one store rather than per agent.** A quota
  /// belongs to the account the harness is logged into: N cards showed one fact
  /// N times at N staleness levels, and a card restored from a project's own
  /// state showed that project's older reading. Nothing here is project-scoped —
  /// the same numbers render whichever project is open, which is also what tells
  /// the user these are not per-project.
  ///
  /// Below the roster rather than above it: the roster is the surface the user
  /// works in and it gets the top of the panel plus all the flexible height,
  /// while this readout keeps its own fixed strip at the foot.
  ///
  /// The right sidebar rather than the left: the projects rail is collapsed
  /// during single-project work, and a passive readout the user wants glanceable
  /// must not disappear with it. This panel is where every other piece of
  /// telemetry already lives.
  import { harnessUsage } from "$lib/state/harnessUsage.svelte";
  import { claudeRateLimitView, codexAccountUsageView, type UsageWindow } from "$lib/usageWindows";
  import { supportsAccountUsageRead } from "$lib/harnessCapabilities";
  import { requestAccountUsageRefresh } from "$lib/state/accountUsage.svelte";
  import { ALL_HARNESSES, HARNESS_LABEL } from "$lib/harnessDisplay";
  import {
    formatResetCountdown,
    formatResetDateTime,
    formatUsedPercent,
    relativeTime,
    usedPercentDigits,
  } from "$lib/utils";
  import HarnessIcon from "$lib/components/ui/HarnessIcon.svelte";
  import Meter from "$lib/components/ui/Meter.svelte";
  import Tooltip from "$lib/components/ui/Tooltip.svelte";
  import type { HarnessKind } from "$lib/types";

  /// One harness's rows, derived from its opaque payload by the per-harness
  /// reader. `now` is read once per derivation for the reset-passed gate; a reset
  /// elapsing while the app sits open won't drop its row until the next
  /// re-render, which is acceptable for a passive readout.
  type Row = {
    harness: HarnessKind;
    windows: UsageWindow[];
    /// Claude's separate billing escalation. Not a window: it says what is being
    /// charged rather than how full a quota is.
    overage: { resetsAtMs: number | null } | null;
    /// Bare reset line for a Claude payload with no window map at all, dated by
    /// the reading it came from. **The one reading-level instant left**, and it is
    /// not an exception to the per-window rule: this line is not a window, and it
    /// is drawn from the newest payload by definition, so the reading's own
    /// measurement is exactly what dates it.
    fallback: { label: string; resetsAtMs: number; measuredAt: string | undefined } | null;
  };

  /// **Opening the panel refreshes it**, and that is the whole claim. This
  /// component is mounted only while the sidebar is open on this mode, so
  /// mounting is the moment these meters become visible.
  ///
  /// **Nothing here reacts to time passing, deliberately.** The rows derive
  /// from the stored reading alone, so while the panel stays open a countdown
  /// keeps whatever text it had when the reading landed and a window whose
  /// reset elapses does not drop. A clock that re-derived on an interval would
  /// fix both; it was considered and declined, because the reading is dated for
  /// the user ("Measured … ago") and a stale countdown beside a stated
  /// measurement time is legible rather than misleading.
  ///
  /// The consequence to know: a user waiting out a reset with the panel open
  /// sees the numbers from their last refresh until something triggers another
  /// one, and ending a turn is the trigger they cannot reach while blocked.
  ///
  /// Reads no reactive state, so it runs once per mount rather than on every
  /// change to the rows below.
  $effect(() => {
    requestAccountUsageRefresh();
  });

  const rows = $derived.by((): Row[] => {
    const now = Date.now();
    const built: Row[] = [];
    for (const harness of ALL_HARNESSES) {
      const reading = harnessUsage[harness];
      if (reading === undefined) continue;
      // Selected by capability rather than by name: this branch reads the
      // account payload, which exists only for a harness Switchboard can ask
      // directly. Matching on "codex" here would decide by harness name the one
      // thing the capability mirror exists to decide.
      if (supportsAccountUsageRead(harness)) {
        const view = codexAccountUsageView(reading.payload, now);
        if (view.windows.length > 0) {
          built.push({
            harness,
            // Codex's windows all come from one call, so they share the reading's
            // instant. Attached per window anyway, so both harnesses' tooltips
            // have one shape rather than changing layout depending on whether the
            // values happen to agree — and here they truthfully do.
            windows: view.windows.map((w) => ({ ...w, measuredAt: reading.observed_at })),
            overage: null,
            fallback: null,
          });
        }
        continue;
      }
      if (harness === "claude_code") {
        // Windows come from the store's retained set, not from the payload: a
        // Claude reading names only the windows its turn's model touched. The
        // payload still supplies the account-level state — the overage escalation
        // and the no-window-map fallback — which only the newest reading speaks
        // for.
        const view = claudeRateLimitView(reading.payload, reading.windows, now);
        if (view !== null) {
          built.push({
            harness,
            windows: view.windows,
            overage: view.overage,
            fallback:
              view.fallback === null ? null : { ...view.fallback, measuredAt: reading.observed_at },
          });
        }
        continue;
      }
      // A harness with no quota surface contributes no row rather than an empty
      // one. Antigravity reports nothing by decision, not by absence.
    }
    return built;
  });

  /// Width of the percentage column, in digits, taken from the widest value
  /// **across the whole section** rather than per harness. The rows read as one
  /// stacked list, so a three-digit reading on one harness has to widen the
  /// column on the other or their detail text stops lining up. Computed rather
  /// than fixed at three so a section that never reaches 100% is not permanently
  /// indented for a value it does not contain.
  const percentDigits = $derived(
    Math.max(
      1,
      ...rows.flatMap((row) =>
        row.windows
          .filter((w) => Number.isFinite(w.usedFraction))
          .map((w) => usedPercentDigits(w.usedFraction).length),
      ),
    ),
  );
</script>

{#if rows.length > 0}
  <!-- Fixed block, deliberately outside the roster's scroll container: these
       meters exist to be glanceable, and scrolling a long agent list must not
       carry them off screen. `shrink-0` beside the roster section's `flex-1`
       is what pins it to the panel's foot — the roster absorbs every spare
       pixel and scrolls when it runs out, this strip keeps its content height. -->
  <section class="border-border/80 shrink-0 border-t px-2 pb-2" data-testid="harness-usage">
    <div
      class="text-muted flex h-8 items-center px-1 text-[11px] leading-none font-semibold tracking-wide uppercase"
    >
      Usage limits
    </div>
    <div class="flex flex-col gap-2">
      {#each rows as row (row.harness)}
        <Tooltip side="left">
          {#snippet trigger(props)}
            <!-- tabindex so keyboard users can reach the reset dates; a div with
                 no click action isn't focusable on its own. -->
            <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
            <div
              {...props}
              tabindex="0"
              class="cursor-default space-y-1 rounded-md px-1 py-0.5 text-xs"
              data-testid={`harness-usage-${row.harness}`}
            >
              <!-- `mb-2` against the list's own `space-y-1`: adjacent margins
                   collapse in block flow, so the effective gap under the name is
                   8px rather than the 4px between meters. That is what makes the
                   name read as a heading for the rows beneath it instead of as
                   another row in the same rhythm. -->
              <div class="mb-2 flex items-center gap-1.5">
                <HarnessIcon harness={row.harness} size="sm" class="h-3.5 w-3.5" />
                <span class="text-fg text-[11px] font-medium">{HARNESS_LABEL[row.harness]}</span>
              </div>
              {#each row.windows as w (w.key)}
                <Meter
                  label={w.label}
                  value={w.usedFraction}
                  detail={w.resetsAtMs === null ? undefined : formatResetCountdown(w.resetsAtMs)}
                  separateDetail
                  {percentDigits}
                  tone={w.surpassedThreshold === undefined && w.limitReached === undefined
                    ? "neutral"
                    : "warning"}
                  testid="harness-usage-window"
                />
              {/each}
              {#if row.fallback !== null}
                <div class="text-fg" data-testid="harness-usage-fallback">
                  {row.fallback.label} resets {formatResetCountdown(row.fallback.resetsAtMs)}
                </div>
              {/if}
              {#if row.overage !== null}
                <!-- -ml-1 offsets the glyph's left-side bearing so it aligns
                     with the text column above. -->
                <div class="text-warning -ml-1" data-testid="harness-usage-overage">
                  ⚡ using credits
                </div>
              {/if}
            </div>
          {/snippet}
          <div
            class="min-w-64 space-y-2.5 text-[13px]"
            data-testid={`harness-usage-detail-${row.harness}`}
          >
            <p class="font-medium">{HARNESS_LABEL[row.harness]} usage</p>
            {#each row.windows as w (w.key)}
              <section class="space-y-1">
                <div class="flex items-baseline gap-4">
                  <span class="min-w-0 font-medium">{w.label}</span>
                  <span class="ml-auto shrink-0 tabular-nums">
                    {formatUsedPercent(w.usedFraction)} used
                  </span>
                </div>
                {#if w.resetsAtMs !== null}
                  <div class="text-primary-fg/70 grid grid-cols-[auto_1fr] gap-4 text-[12px]">
                    <span>Resets</span>
                    <span class="text-right tabular-nums">{formatResetDateTime(w.resetsAtMs)}</span>
                  </div>
                {/if}
                <!-- Per window, not per card. Retained windows are measured by
                     the turns that touch them, so a weekly cap read on Tuesday
                     genuinely sits beside a 5-hour window read a minute ago and
                     one shared line would be false for all but one of them.
                     Omitted when the window carries no instant — the clean-hide
                     rule the rest of this surface follows, and the reason the
                     instant is nullable rather than sentinel-filled. -->
                {#if w.measuredAt !== undefined}
                  <div class="text-primary-fg/70 grid grid-cols-[auto_1fr] gap-4 text-[12px]">
                    <span>Measured</span>
                    <span class="text-right" data-testid="harness-usage-measured">
                      {relativeTime(w.measuredAt)}
                    </span>
                  </div>
                {/if}
              </section>
            {/each}
            {#if row.fallback !== null}
              <div class="grid grid-cols-[auto_1fr] gap-4">
                <span>{row.fallback.label}</span>
                <span class="text-right tabular-nums">
                  Resets {formatResetDateTime(row.fallback.resetsAtMs)}
                </span>
              </div>
              {#if row.fallback.measuredAt !== undefined}
                <div class="text-primary-fg/70 grid grid-cols-[auto_1fr] gap-4 text-[12px]">
                  <span>Measured</span>
                  <span class="text-right" data-testid="harness-usage-measured">
                    {relativeTime(row.fallback.measuredAt)}
                  </span>
                </div>
              {/if}
            {/if}
            {#if row.overage !== null}
              <div class="text-warning border-primary-fg/20 border-t pt-2">
                <p class="font-medium">Spending usage credits</p>
                {#if row.overage.resetsAtMs !== null}
                  <p class="mt-0.5 text-[12px]">
                    Overage window resets {formatResetDateTime(row.overage.resetsAtMs)}
                  </p>
                {/if}
              </div>
            {/if}
            <!-- States the refresh rule rather than an instruction to send a
                 message. That imperative was wrong in exactly the situation these
                 meters exist for: a turn refreshes only the limits its own model
                 draws on, so a user staring at a stale model-gated cap would send
                 a message and watch the instant not move. The rule explains both
                 why the times above can differ and what moves them. -->
            <p
              class="text-primary-fg/70 border-primary-fg/20 border-t pt-2 text-[12px]"
              data-testid="harness-usage-refresh-rule"
            >
              Each limit updates when a turn runs against it.
            </p>
          </div>
        </Tooltip>
      {/each}
    </div>
  </section>
{/if}
