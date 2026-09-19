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
  /// **Both harnesses now state this themselves, and neither measurement is
  /// overridden.** Claude names the blocked window in the same payload that
  /// refuses; Codex names it per bucket in `rateLimitReachedType`. Each reports
  /// that bucket's real utilization alongside, so the number stands as measured
  /// and only the tone is added.
  ///
  /// It used to mean something weaker on Codex — a verdict carried beside the
  /// reading, attributed by guess to the most-used window and drawn full,
  /// because the per-turn payload named no window and recorded no measurement
  /// on a refusal. Reading the account directly removed both the guess and the
  /// painting.
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
    // **`rejected` is overloaded and does not mean blocked on its own.** An
    // **overage** turn carries the same status (§1.4: `isUsingOverage:true` plus
    // `status:"rejected"` plus `overageResetsAt`) and is *served* — the quota is
    // spent and Anthropic is billing credits for the work it is still doing.
    // Flagging that window would sit the card permanently in the warning tone for
    // anyone routinely in overage, and make the one state where work is actually
    // refused indistinguishable from the state where it is not. The escalation
    // beneath the meters already says what is happening there.
    //
    // The one captured wall (2026-09-18, the Fable weekly cap) reports
    // `isUsingOverage: false` with overage disabled at the org level, so the two
    // states separate on this field in the only observation we have.
    //
    // **Nothing here overrides the measurement**, unlike the Codex reader. Codex
    // records a windowless payload on a refused turn, so its last number is
    // stale and the refusal is the only truthful thing left; Claude reports the
    // blocked window's own utilization in the same payload that refuses, so the
    // number is already right and only the tone was missing. A window named here
    // but outside `CLAUDE_WINDOWS` drops with its flag, exactly as a threshold
    // warning does.
    const refused =
      p.status === "rejected" && p.isUsingOverage !== true && typeof p.rateLimitType === "string"
        ? p.rateLimitType
        : undefined;
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

/// Defensive read of Codex's **account quota** payload — the response of
/// `account/rateLimits/read`, stored under its own wrapping as
/// `{ordinaryUsageAllowed, rateLimitsByLimitId}`.
///
/// **This replaced a reader that could not tell which quota it held.** Codex's
/// per-turn rollout reports one bucket under identical identifiers whichever
/// limit it describes, so the old path had to assert a scope it could not check
/// and paint a guessed window full on a refusal. Reading the account is what
/// makes the scope knowable: each bucket declares its model association, and
/// each states its own exhaustion.
///
/// **Which buckets render: the account-wide ones, identified by having no
/// associated model.** A bucket carrying a `normalModelSlug` is a
/// model-specific reserve (the observed one is `gpt-reserve` on
/// `gpt-5.6-luna`), and showing it beside an account allowance invites reading
/// a reserve's headroom as the quota that governs ordinary work.
///
/// Filtering on the model association rather than on `limitId === "codex"` is
/// deliberate: the identifier is not guaranteed across plans, and this rule
/// keeps working on a plan carrying both a 5-hour and a weekly account limit —
/// which is how Claude's section already behaves.
///
/// **A bucket missing `normalModelSlug` entirely is skipped, not treated as
/// account-wide.** Were the field renamed upstream, treating absence as "no
/// model" would promote every reserve into the account section and label it as
/// an ordinary allowance — reintroducing, in a new place, the exact defect this
/// milestone removes. Skipping instead renders nothing, which is the direction
/// taken throughout: say less rather than something wrong.
export function codexAccountUsageView(payload: unknown, nowMs: number): UsageWindow[] {
  if (typeof payload !== "object" || payload === null) return [];
  const buckets = (payload as { rateLimitsByLimitId?: unknown }).rateLimitsByLimitId;
  if (typeof buckets !== "object" || buckets === null) return [];
  const windows: UsageWindow[] = [];
  for (const [limitId, bucket] of Object.entries(buckets as Record<string, unknown>)) {
    if (typeof bucket !== "object" || bucket === null) continue;
    const b = bucket as {
      limitName?: unknown;
      normalModelSlug?: unknown;
      primary?: unknown;
      rateLimitReachedType?: unknown;
    };
    // Present *and* null — see the note above on why absence is not "null".
    if (!("normalModelSlug" in b) || b.normalModelSlug !== null) continue;
    const w = b.primary;
    if (typeof w !== "object" || w === null) continue;
    const ww = w as { usedPercent?: unknown; resetsAt?: unknown; windowDurationMins?: unknown };
    if (typeof ww.usedPercent !== "number") continue;
    // A windowless bucket is a shape Codex actually emits (`limit_id:
    // "premium"` arrives with both windows null on a refused turn), so it is
    // skipped rather than rendered as a bar with no value.
    let resetsAtMs: number | null = null;
    if (typeof ww.resetsAt === "number") {
      const ms = ww.resetsAt * 1000;
      // Reset passed → the window cycled and this percentage describes the
      // window before it, so the bucket drops while its siblings stay.
      if (ms <= nowMs) continue;
      resetsAtMs = ms;
    }
    windows.push({
      key: limitId,
      label: codexBucketLabel(ww.windowDurationMins, b.limitName),
      usedFraction: ww.usedPercent / 100,
      resetsAtMs,
      limitReached: isExhausted(b.rateLimitReachedType, ww.usedPercent) ? true : undefined,
    });
  }
  return windows;
}

/// Name an **account-wide** bucket, using the shared cross-harness vocabulary.
///
/// **This is the same string Claude's `seven_day` row uses, and that is the
/// point.** A user reading the two harnesses' rows side by side should be
/// comparing quantities, not decoding two vocabularies for one idea.
///
/// **Why "all models" is a reading of the data here and was an invention
/// before.** The old rollout path received a single unnamed bucket and could not
/// tell the account allowance from the model reserve, so calling it "all models"
/// asserted something that might have been false — that was the original defect.
/// This reader only ever labels buckets that passed the `normalModelSlug === null`
/// filter, so the bucket in hand is *by construction* the one with no model
/// association. The duration is likewise stated by the payload
/// (`windowDurationMins`), not guessed. Both halves of the label are read rather
/// than assumed.
///
/// Called only for account-wide buckets. Were model-scoped ones ever rendered,
/// they would take `Weekly · <model>`, exactly as Claude's per-model rows do.
///
/// Falls back to Codex's own `limitName` for a duration we do not recognize —
/// the account-wide bucket carries `limitName: null` in every capture, so this
/// is for a future shape rather than today's — and to a neutral noun when there
/// is nothing to go on. What it never does is invent a duration or a scope.
function codexBucketLabel(windowDurationMins: unknown, limitName: unknown): string {
  if (windowDurationMins === 10080) return LABEL_WEEKLY_ALL;
  if (windowDurationMins === 300) return LABEL_FIVE_HOUR;
  return typeof limitName === "string" && limitName !== "" ? limitName : "Quota";
}

/// Whether a bucket should draw as exhausted.
///
/// Codex states this itself in `rateLimitReachedType`, so the verdict is read
/// rather than derived — no percentage threshold of ours decides it.
///
/// **The `usedPercent > 0` conjunct guards a stale flag on a refreshed window.**
/// A probe against a recovered account (2026-09-19) found the flag clears on
/// reset, so this currently guards a shape Codex does not produce; it stays
/// because it is free and still covers the windowless bucket. Be honest about
/// two things. It **infers recovery from a percentage**, which the protocol
/// schema explicitly tells clients not to do — accepted knowingly as the
/// narrowest guard against a failure mode we cannot rule out from one
/// observation. And it errs toward **understating** a quota, which is this
/// codebase's direction throughout: a meter that fails to shout is recoverable,
/// one that falsely claims you are blocked is not.
///
/// Its coverage is narrow and worth stating: it only catches a read taken
/// before any turn runs in the new window. One turn in, `usedPercent` is a few
/// percent and a stale flag would sail through. Since a turn ending is itself
/// what triggers a read, what this reliably protects is the read a *waiting*
/// user is most likely to be looking at.
function isExhausted(rateLimitReachedType: unknown, usedPercent: number): boolean {
  return rateLimitReachedType !== null && rateLimitReachedType !== undefined && usedPercent > 0;
}
