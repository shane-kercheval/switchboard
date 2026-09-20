<script lang="ts">
  /// A "how full is this" gauge: label on the left, optional detail plus the
  /// used percentage on the right, a track-and-fill bar beneath. Every usage
  /// gauge on an agent card renders through this one component — the context
  /// meter, each rate-limit window, each context-breakdown category — so the
  /// geometry and the percentage rounding are defined once instead of drifting
  /// per cell.
  ///
  /// No tooltip inside: some call sites want one carrying reset dates or
  /// snapshot age, others want none, and a primitive that owned hover would
  /// force the question. Wrap it where you need it.
  import { cn, formatUsedPercent, usedPercentDigits } from "$lib/utils";

  type Tone = "neutral" | "warning";

  type Props = {
    label: string;
    /// Fraction **used**, 0–1 — never remaining. Both wired harnesses report
    /// used, the Claude desktop app shows used, and a source that reports
    /// remaining is converted where it is derived so every meter in the app
    /// means the same thing. A primitive that accepted either would make each
    /// call site's meaning something you had to go look up.
    value: number;
    /// Secondary right-hand text (e.g. "121k / 1M"), before the percentage.
    detail?: string;
    /// Visually separate the detail from the percentage with a middle dot.
    separateDetail?: boolean;
    /// Reserve a right-aligned column this many **digits** wide for the
    /// percentage, so a group of meters lines up. Absent means no reservation.
    ///
    /// **Digits, not characters, and supplied by the caller rather than assumed.**
    /// The percent sign is wider than a digit even under `tabular-nums`, so a
    /// reservation that includes it cannot be expressed exactly in `ch` — the
    /// previous fixed four-character column fitted "70%" and was overflowed by
    /// "100%", which pushed that row's detail text left of its neighbours'. The
    /// sign is rendered outside the reserved box, leaving only tabular digits
    /// inside, where `ch` is exact. The caller passes the widest digit count in
    /// its group so a group that never reaches three digits is not indented for
    /// a value it does not contain.
    percentDigits?: number;
    /// `warning` fills with the caution token. Reserved for a threshold the
    /// harness itself reports having passed — not a percentage we pick, which
    /// would make the same occupancy alarming on one harness and calm on
    /// another.
    tone?: Tone;
    /// Also names the fill, as `<testid>-fill`.
    testid?: string;
    class?: string;
  };

  let {
    label,
    value,
    detail,
    separateDetail = false,
    percentDigits,
    tone = "neutral",
    testid,
    class: className,
  }: Props = $props();

  /// Only the fill clamps. The percentage text reports what the source said,
  /// including over 100% — a quota that says 103% used is telling the user
  /// something real, and a bar cannot draw it.
  const fillPercent = $derived(Math.min(Math.max(value, 0), 1) * 100);

  /// A non-finite value renders nothing at all rather than a number. Without
  /// this the invalid width declaration is dropped and the fill falls back to
  /// auto — a completely full bar labelled "NaN%", which tells the user their
  /// quota is spent when the truth is that it is unknown. Hiding follows the
  /// card's convention that an impossible measurement clean-hides instead of
  /// masquerading as a plausible one; call sites remain responsible for
  /// filtering values they know to be junk.
  const measurable = $derived(Number.isFinite(value));
</script>

{#if measurable}
  <!-- Size lives on the wrapper so `class` can raise it; the label row inherits
       rather than restating it, which is what lets a caller pass `text-xs`
       without the inner size silently winning. -->
  <div class={cn("min-w-0 text-[11px]", className)} data-testid={testid}>
    <div class="text-muted mb-0.5 flex items-baseline gap-2">
      <span class="min-w-0 truncate">{label}</span>
      <span class="ml-auto flex shrink-0 items-baseline gap-1.5 tabular-nums">
        {#if detail !== undefined}
          <span>{detail}</span>
          {#if separateDetail}<span aria-hidden="true">·</span>{/if}
        {/if}
        {#if percentDigits === undefined}
          <span>{formatUsedPercent(value)}</span>
        {:else}
          <!-- The width is computed per group, so it cannot be a Tailwind class:
               Tailwind scans for whole class strings and never generates a
               composed one. `inline-block` is what lets a min-width apply. -->
          <span class="whitespace-nowrap"
            ><span class="inline-block text-right" style:min-width="{percentDigits}ch"
              >{usedPercentDigits(value)}</span
            >%</span
          >
        {/if}
      </span>
    </div>
    <div class="bg-active h-1 w-full overflow-hidden rounded">
      <div
        class={cn("h-full", tone === "warning" ? "bg-warning" : "bg-fg")}
        style:width="{fillPercent.toFixed(1)}%"
        data-testid={testid === undefined ? undefined : `${testid}-fill`}
      ></div>
    </div>
  </div>
{/if}
