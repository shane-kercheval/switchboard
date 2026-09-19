<script lang="ts">
  /// Account-scoped quota meters, one row per harness that has reported a
  /// reading.
  ///
  /// **Above the agent roster rather than on each agent card, and fed from one
  /// store rather than per agent.** A quota belongs to the account the harness is
  /// logged into: N cards showed one fact N times at N staleness levels, and a
  /// card restored from a project's own state showed that project's older
  /// reading. Nothing here is project-scoped — the same numbers render whichever
  /// project is open, which is also what tells the user these are not
  /// per-project.
  ///
  /// The right sidebar rather than the left: the projects rail is collapsed
  /// during single-project work, and a passive readout the user wants glanceable
  /// must not disappear with it. This panel is where every other piece of
  /// telemetry already lives.
  import { harnessUsage } from "$lib/state/harnessUsage.svelte";
  import { claudeRateLimitView, codexRateLimitView, type UsageWindow } from "$lib/usageWindows";
  import { ALL_HARNESSES, HARNESS_LABEL } from "$lib/harnessDisplay";
  import {
    formatResetCountdown,
    formatResetDateTime,
    formatUsedPercent,
    relativeTime,
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
    /// Bare reset line for a Claude payload with no window map at all.
    fallback: { label: string; resetsAtMs: number } | null;
    /// When the harness measured the reading, for the age line. Always present:
    /// every reading is ranked by this instant, so every reading can date itself.
    measuredAt: string;
  };

  const rows = $derived.by((): Row[] => {
    const now = Date.now();
    const built: Row[] = [];
    for (const harness of ALL_HARNESSES) {
      const reading = harnessUsage[harness];
      if (reading === undefined) continue;
      const measuredAt = reading.observed_at;
      if (harness === "codex") {
        const windows = codexRateLimitView(reading.payload, now, reading.limit_reached === true);
        if (windows.length > 0) {
          built.push({ harness, windows, overage: null, fallback: null, measuredAt });
        }
        continue;
      }
      if (harness === "claude_code") {
        // Claude states a refusal in the payload, so no verdict is passed in
        // here — see `claudeRateLimitView`.
        const view = claudeRateLimitView(reading.payload, now, reading.model);
        if (view !== null) {
          built.push({
            harness,
            windows: view.windows,
            overage: view.overage,
            fallback: view.fallback,
            measuredAt,
          });
        }
        continue;
      }
      // A harness with no quota surface contributes no row rather than an empty
      // one. Antigravity reports nothing by decision, not by absence.
    }
    return built;
  });
</script>

{#if rows.length > 0}
  <!-- Fixed block, deliberately outside the roster's scroll container: these
       meters exist to be glanceable, and scrolling a long agent list must not
       carry them off screen. -->
  <section class="border-border/80 shrink-0 border-b px-2 pb-2" data-testid="harness-usage">
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
              <div class="flex items-center gap-1.5">
                <HarnessIcon harness={row.harness} size="sm" class="h-3.5 w-3.5" />
                <span class="text-fg text-[11px] font-medium">{HARNESS_LABEL[row.harness]}</span>
              </div>
              {#each row.windows as w (w.key)}
                <Meter
                  label={w.label}
                  value={w.usedFraction}
                  detail={w.resetsAtMs === null ? undefined : formatResetCountdown(w.resetsAtMs)}
                  separateDetail
                  alignPercentage
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
              </section>
            {/each}
            {#if row.fallback !== null}
              <div class="grid grid-cols-[auto_1fr] gap-4">
                <span>{row.fallback.label}</span>
                <span class="text-right tabular-nums">
                  Resets {formatResetDateTime(row.fallback.resetsAtMs)}
                </span>
              </div>
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
            <!-- Shown for every harness and every reading, not just a restored
                 stream-only snapshot. A session-file-backed reading is durable,
                 which was mistaken for current: it is re-read on every open but
                 the file itself can be days old, and an account-level reading is
                 only as fresh as the last turn *any* agent ran. Age is the
                 question a reader actually has, so it is always answered. -->
            <p
              class="text-primary-fg/70 border-primary-fg/20 border-t pt-2 text-[12px]"
              data-testid="harness-usage-measured"
            >
              Measured {relativeTime(row.measuredAt)} — send a message to refresh.
            </p>
          </div>
        </Tooltip>
      {/each}
    </div>
  </section>
{/if}
