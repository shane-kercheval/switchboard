/// Usage-window derivation for the agent card: each harness's opaque
/// rate-limit payload read defensively into the one shape the meters render.
///
/// Lives outside `Sidebar.svelte` because it is pure — no reactive state, no
/// component dependencies — so each rule can be tested directly instead of
/// through a rendered agent card. The component keeps the markup, the tooltip
/// content, and the per-harness gating.
import { claudeModelFamilyLabel } from "$lib/agentSelection";

/// One usage window as the card draws it. `usedFraction` is 0–1 **used**,
/// converted here rather than at the meter: Codex reports 0–100 and Claude a
/// 0–1 fraction, and a primitive that accepted either would make every call
/// site's meaning ambiguous.
export type UsageWindow = {
  key: string;
  label: string;
  usedFraction: number;
  resetsAtMs: number | null;
  /// Set only when the harness itself reported passing a threshold — never a
  /// percentage we pick, which would make the same occupancy alarming on one
  /// harness and calm on the other.
  surpassedThreshold?: number;
  /// Set on the window a refusal applies to, which is the one signal that draws a
  /// meter in the warning tone without the harness having reported a threshold.
  ///
  /// **How it is established differs per harness, and so does its effect on
  /// `usedFraction`.** Claude names the blocked window and reports its real
  /// utilization in the same payload that refuses, so the measurement stands as
  /// measured. Codex names no window and records no measurement on a refused turn,
  /// so the window is inferred and drawn full — the last measurement may read 93%
  /// while the harness has since said no, and a bar still reading 93% beside a
  /// refusal says the meter is wrong. See each harness's reader for the detail.
  limitReached?: true;
};

/// Shared across both harnesses (decision 10): the same window gets the same
/// words wherever it came from, so a user reading two cards side by side is
/// comparing quantities rather than decoding vocabularies.
const LABEL_FIVE_HOUR = "5-hour limit";
const LABEL_WEEKLY_ALL = "Weekly · all models";

/// Claude's windows in render order, each with its label. **One ordered list
/// rather than an order array plus a lookup map**, so a key cannot be added
/// without a label — the drift that pairing removes was previously covered by
/// a runtime guard that no input could reach. `null` marks the one window
/// whose label comes from the observed model (below).
///
/// The CLI binary's own key list (`strings`, 2.1.274) also carries
/// `seven_day_cowork`, `seven_day_omelette`, and `seven_day_oauth_apps`. None
/// is a Claude Code window on any plan we can probe, and a junk label is
/// worse than a dropped window — iterating this list rather than the
/// payload's keys is what drops them. Extend here when a probe names one.
const CLAUDE_WINDOWS: ReadonlyArray<{ key: string; label: string | null }> = [
  { key: "five_hour", label: LABEL_FIVE_HOUR },
  { key: "seven_day", label: LABEL_WEEKLY_ALL },
  { key: "seven_day_overage_included", label: null },
  { key: "seven_day_opus", label: "Weekly · Opus" },
  { key: "seven_day_sonnet", label: "Weekly · Sonnet" },
];

/// `seven_day_overage_included` is the weekly cap for a server-side allowlist
/// of models that the payload never names. It arrives only on turns run
/// against one of those models, so the model that delivered the snapshot is a
/// truthful label for it — by family name, since the raw id is wider than the
/// column. After a reload there is no observed model (the sidecar
/// deliberately doesn't carry one) and the window says so rather than naming
/// a model it can't vouch for.
function modelWeeklyLabel(model: string | undefined): string {
  return model === undefined || model === ""
    ? "Weekly · model-specific"
    : `Weekly · ${claudeModelFamilyLabel(model)}`;
}

/// Human label for Claude's *top-level* window, used only by the no-
/// `unifiedWindows` fallback below. Derived from `rateLimitType` rather than
/// hardcoded: the event tells us the window kind, so we don't assert a
/// duration it could contradict.
function rateLimitLabel(rateLimitType: unknown): string {
  return rateLimitType === "five_hour" ? LABEL_FIVE_HOUR : "rate limit";
}

/// Defensive read of Claude's opaque `last_rate_limit` payload.
///
/// `unifiedWindows` is authoritative whenever it is present — it carries
/// every window the desktop app shows, each with a used fraction. The
/// top-level `resetsAt` / `rateLimitType` pair is a **fallback only**, for an
/// older CLI (or a future one that drops the field): it has no percentage, so
/// it renders as a bare reset line rather than a meter. A bar with no value
/// would be a blank bar, which the card's clean-hide convention forbids more
/// than it forbids a missing bar. The fallback is not dead code; the field is
/// undocumented and could vanish without notice.
///
/// Every window is gated on its own reset being in the *future*. A reset is
/// an absolute timestamp, so it stays accurate however old the snapshot is —
/// right until `nowMs` passes it, at which point that window has cycled and
/// we don't have its new reset, so it drops while its siblings stay.
///
/// `overage` is the separate "using credits" escalation (`isUsingOverage`),
/// about what is being *billed* rather than how full a window is. Its own
/// window can be days out, so it lives in the tooltip. A null overage reset
/// ("flag set, no window time") is still shown — we can't prove it stale.
///
/// Returns `null` when nothing is displayable.
export function claudeRateLimitView(
  payload: unknown,
  nowMs: number,
  model: string | undefined,
): {
  windows: UsageWindow[];
  fallback: { label: string; resetsAtMs: number } | null;
  overage: { resetsAtMs: number | null } | null;
} | null {
  if (typeof payload !== "object" || payload === null) return null;
  const p = payload as {
    status?: unknown;
    rateLimitType?: unknown;
    surpassedThreshold?: unknown;
    resetsAt?: unknown;
    isUsingOverage?: unknown;
    overageResetsAt?: unknown;
    unifiedWindows?: unknown;
  };

  // An **empty** container counts as absent: it reported nothing, so the
  // top-level fallback is still the best available signal. A *non-empty*
  // container whose entries were all dropped (reset-passed, unreadable
  // fraction, or a key we deliberately exclude) stays authoritative and the
  // cell clean-hides — those windows were filtered on purpose, and falling
  // back there would override the per-window rules rather than fill a gap.
  const unified = p.unifiedWindows;
  const hasUnified =
    typeof unified === "object" && unified !== null && Object.keys(unified).length > 0;
  const windows: UsageWindow[] = [];
  if (hasUnified) {
    // The threshold flag names its window in `rateLimitType` and its level in
    // `surpassedThreshold`. A turn can emit a second event carrying the
    // superset, so last-write-wins on the payload is what makes this correct.
    //
    // A flag naming a window outside `CLAUDE_WINDOWS` is dropped with that
    // window, losing the signal. Unobserved (the rendered keys cover every
    // window any probe has seen) and deliberately not backfilled with a
    // generic amber line — recorded under the plan's known limitations.
    const flagged =
      p.status === "allowed_warning" && typeof p.rateLimitType === "string"
        ? p.rateLimitType
        : undefined;
    const threshold = typeof p.surpassedThreshold === "number" ? p.surpassedThreshold : undefined;
    // **The wall, which is a different status from the warning.** At a warning
    // Claude sends `allowed_warning` plus a numeric `surpassedThreshold`; at a
    // refusal it sends `rejected` and no threshold at all, so the warning path
    // above leaves every window unflagged and the meter draws a spent quota in
    // the neutral tone. `rateLimitType` names the window that did the blocking in
    // both cases.
    //
    // **Nothing here overrides the measurement**, unlike the Codex reader. Codex
    // records a windowless payload on a refused turn, so its last number is
    // stale and the refusal is the only truthful thing left; Claude reports the
    // blocked window's own utilization in the same payload that refuses, so the
    // number is already right and only the tone was missing. A window named here
    // but outside `CLAUDE_WINDOWS` drops with its flag, exactly as a threshold
    // warning does.
    const refused =
      p.status === "rejected" && typeof p.rateLimitType === "string" ? p.rateLimitType : undefined;
    for (const { key, label } of CLAUDE_WINDOWS) {
      const w = (unified as Record<string, unknown>)[key];
      if (typeof w !== "object" || w === null) continue;
      const ww = w as { utilization?: unknown; resetsAt?: unknown };
      if (typeof ww.utilization !== "number") continue;
      if (!(ww.utilization >= 0 && ww.utilization <= 1)) continue;
      if (typeof ww.resetsAt !== "number") continue;
      const resetsAtMs = ww.resetsAt * 1000;
      if (resetsAtMs <= nowMs) continue;
      windows.push({
        key,
        label: label ?? modelWeeklyLabel(model),
        usedFraction: ww.utilization,
        resetsAtMs,
        surpassedThreshold: key === flagged ? threshold : undefined,
        limitReached: key === refused ? true : undefined,
      });
    }
  }

  let fallback: { label: string; resetsAtMs: number } | null = null;
  if (!hasUnified && typeof p.resetsAt === "number") {
    const resetsAtMs = p.resetsAt * 1000;
    if (resetsAtMs > nowMs) fallback = { label: rateLimitLabel(p.rateLimitType), resetsAtMs };
  }

  let overage: { resetsAtMs: number | null } | null = null;
  if (p.isUsingOverage === true) {
    if (typeof p.overageResetsAt === "number") {
      const overageMs = p.overageResetsAt * 1000;
      overage = overageMs > nowMs ? { resetsAtMs: overageMs } : null;
    } else {
      overage = { resetsAtMs: null };
    }
  }

  if (windows.length === 0 && fallback === null && overage === null) return null;
  return { windows, fallback, overage };
}

/// Label for a Codex rate-limit window, from its `window_minutes` duration
/// (300 = the ~5-hour primary, 10080 = the weekly secondary) mapped onto the
/// shared strings above. Unknown/absent durations fall back to "Quota" — a
/// payload carrying only a bare `used_percent` still reads as a real gauge.
function codexWindowLabel(windowMinutes: unknown): string {
  if (windowMinutes === 300) return LABEL_FIVE_HOUR;
  if (windowMinutes === 10080) return LABEL_WEEKLY_ALL;
  return "Quota";
}

/// Whether two Codex rate-limit payloads describe the **same windows**, used to
/// decide whether a recorded refusal still applies to the snapshot on screen.
///
/// The refusal (`AgentRuntime.usage_limit_reached`) is a verdict about a
/// particular window, not about the agent, and `last_rate_limit` is replaced
/// independently of it — so without this the verdict can decorate a snapshot it
/// was never about, drawing a freshly reset quota as spent.
///
/// **Identity is the set of `resets_at` values**, which is what makes the
/// asymmetry work: a refused turn's own enrichment re-emits the *same*
/// pre-cap record (`enrichment.rate_limits` is the last window-*bearing*
/// record in the file, and a refusal appends only windowless ones), so the
/// refusal survives the `TurnEnd → RateLimitEvent` pair that set it, while a
/// genuinely new window does not match and clears it.
///
/// **An indeterminate identity counts as different**, i.e. clears. A payload
/// can render meters while reporting no reset time at all — `codexRateLimitView`
/// keeps such a window deliberately, since staleness can't be proven without
/// one — and there is no way to tell two reset-less windows apart. Treating
/// unknown as "same" would let a stale verdict sit on a reset-less window
/// forever, because the reset-passed gate can never retire it either. The cost
/// is that on a payload reporting no reset times the refusal never takes
/// effect; no Codex version we have observed omits them. This is the same
/// direction taken everywhere else here: understating a quota is safer than
/// telling someone to stop working.
export function sameCodexUsageWindows(a: unknown, b: unknown): boolean {
  const left = codexWindowIdentity(a);
  return left !== null && left === codexWindowIdentity(b);
}

/// `resets_at` of every window-bearing key, sorted, or `null` when any of them
/// is unreadable (see [`sameCodexUsageWindows`] for why unknown is not "same").
function codexWindowIdentity(payload: unknown): string | null {
  if (typeof payload !== "object" || payload === null) return null;
  const resets: number[] = [];
  for (const key of ["primary", "secondary"] as const) {
    const w = (payload as Record<string, unknown>)[key];
    if (typeof w !== "object" || w === null) continue;
    const ww = w as { used_percent?: unknown; resets_at?: unknown };
    if (typeof ww.used_percent !== "number") continue;
    if (typeof ww.resets_at !== "number") return null;
    resets.push(ww.resets_at);
  }
  return resets.length === 0 ? null : resets.sort((x, y) => x - y).join(",");
}

/// Defensive read of Codex's opaque `last_rate_limit` into its independent
/// windows (`primary` + `secondary`). Same reset-passed rule as the Claude
/// reader; a window with no `resets_at` is kept (can't prove it stale — older
/// Codex shapes and minimal fixtures omit it). Codex rate-limit is
/// session-file-backed (class B, durable), so there's no snapshot-age
/// qualifier. Codex reports no threshold flag; the one way a window here
/// warns is `limitReached`.
///
/// `limitReached` is the agent's last turn having been refused for the
/// limit (`FailureKind.usage_limit`). The payload cannot say so itself: a
/// refused turn records a *windowless* payload (kept out of the snapshot, see
/// `session_file.rs::rate_limits_carry_window`), so the snapshot still holds
/// the last measurement — 93%, say — while the harness has since said no.
///
/// **The refusal is attributed to one window, the most-used.** Codex reports
/// that *a* limit was exceeded and never which (`rate_limit_reached_type` is
/// null even on a 100% record), so flagging every surviving window would tell
/// a user who exhausted a 5-hour quota that their weekly one is gone too —
/// days of waiting claimed for an hour of it, which is a worse error than the
/// stale measurement this flag exists to correct. The most-used window is the
/// likeliest culprit, not provably the exhausted one: one large turn can push
/// a short window past its limit from a low last reading while a weekly sits
/// higher. That mis-picks between two windows rather than condemning both,
/// and on a single-window payload it cannot mis-pick at all.
///
/// The reset-passed gate still applies first: once a window has cycled, the
/// refusal is as stale as the measurement, and the window drops with it —
/// which is also why the flag needs no expiry of its own.
///
/// `used_percent / 100` is left unrounded. Rounding at the source would make
/// the rendered percentage byte-match Codex's own TUI at half-percent values,
/// but nobody compares the two, and the bar and the number should be drawn
/// from one value rather than from a figure pre-rounded for a different
/// renderer. Returns `[]` when nothing is displayable.
export function codexRateLimitView(
  payload: unknown,
  nowMs: number,
  limitReached = false,
): UsageWindow[] {
  if (typeof payload !== "object" || payload === null) return [];
  const windows: UsageWindow[] = [];
  for (const key of ["primary", "secondary"] as const) {
    const w = (payload as Record<string, unknown>)[key];
    if (typeof w !== "object" || w === null) continue;
    const ww = w as { used_percent?: unknown; resets_at?: unknown; window_minutes?: unknown };
    if (typeof ww.used_percent !== "number") continue;
    let resetsAtMs: number | null = null;
    if (typeof ww.resets_at === "number") {
      const ms = ww.resets_at * 1000;
      if (ms <= nowMs) continue; // reset-passed → window cycled, % is stale
      resetsAtMs = ms;
    }
    windows.push({
      key,
      label: codexWindowLabel(ww.window_minutes),
      usedFraction: ww.used_percent / 100,
      resetsAtMs,
    });
  }
  if (limitReached && windows.length > 0) {
    // First wins on a tie, so two equally-used windows attribute
    // deterministically rather than by key order elsewhere in the payload.
    const culprit = windows.reduce((a, b) => (b.usedFraction > a.usedFraction ? b : a));
    culprit.usedFraction = 1;
    culprit.limitReached = true;
  }
  return windows;
}
