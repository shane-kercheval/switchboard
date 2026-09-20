/// Usage-window derivation for the agent card: each harness's opaque
/// rate-limit payload read defensively into the one shape the meters render.
///
/// Lives outside `Sidebar.svelte` because it is pure — no reactive state, no
/// component dependencies — so each rule can be tested directly instead of
/// through a rendered agent card. The component keeps the markup, the tooltip
/// content, and the per-harness gating.
import { claudeModelFamilyLabel } from "$lib/agentSelection";
import type { HarnessKind, TurnId } from "$lib/types";

/// One usage window as the card draws it. `usedFraction` is 0–1 **used**,
/// converted here rather than at the meter: Codex reports 0–100 and Claude a
/// 0–1 fraction, and a primitive that accepted either would make every call
/// site's meaning ambiguous.
export type UsageWindow = {
  key: string;
  label: string;
  usedFraction: number;
  resetsAtMs: number | null;
  /// When this window was measured, ISO-8601.
  ///
  /// **Per window rather than per card.** Windows are retained individually, so
  /// they genuinely do not share one instant: a weekly cap measured on Tuesday
  /// sits beside a 5-hour window measured a minute ago. A single card-level line
  /// would be true of one row and false of the rest.
  ///
  /// Absent when the delivering reading carried no instant, and for a window
  /// restored from a file written before instants were held per window. No age is
  /// claimed rather than one borrowed from a different window's measurement.
  measuredAt?: string;
  /// Set only when the harness itself reported passing a threshold — never a
  /// percentage we pick, which would make the same occupancy alarming on one
  /// harness and calm on the other.
  surpassedThreshold?: number;
  /// Draw this meter in the warning tone. **The two readers set it on different
  /// grounds, and the weaker one is Codex's.**
  ///
  /// - **Claude: the payload named this window as the refused one.** Claude
  ///   reports that window's real utilization alongside the refusal, so the
  ///   number stands as measured and only the tone is added.
  /// - **Codex: the measurement reached 100%.** That is our inference from a
  ///   number, not something Codex said. Codex does send a reason code, and this
  ///   reader deliberately ignores it — see `codexAccountUsageView`.
  ///
  /// Both are stated because this milestone exists on account of a field whose
  /// name implied a claim the data did not support. One name spanning two
  /// strengths of evidence is exactly that hazard, so the difference is written
  /// down rather than left to the name.
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

/// One window as the store holds it: the vendor's own window object, verbatim,
/// plus the context of the reading that delivered it.
///
/// **Nothing stored here is a conclusion**, and that is load-bearing rather than
/// stylistic. Every displayed number, label, and tone is re-derived from these
/// fields at render, so a mistaken interpretation is corrected *retroactively*
/// for windows already on disk — twice now, that property is the only reason a
/// bad reading did not have to be waited out. Deriving flags at ingest instead
/// would freeze the judgement into `usage.yaml` until each window reset.
export type StoredUsageWindow = {
  /// `unifiedWindows[key]`, exactly as Claude sent it, and **not validated
  /// here**: a window whose fraction or reset cannot be read is stored anyway
  /// and dropped at render, so tightening that read later applies to what is
  /// already stored.
  window: unknown;
  /// The account-level fields of the delivering reading that this window's tone
  /// is derived from, verbatim.
  ///
  /// **Held per window rather than read from the newest reading, which is the
  /// point of merging at all.** Claude's `status` is account-level, so an
  /// `allowed` reading delivered by an Opus turn is evidence about the windows
  /// that turn measured and says nothing about a model-gated cap it never
  /// mentioned. Deriving that cap's tone from it would clear a refusal nothing
  /// re-measured.
  status?: unknown;
  rate_limit_type?: unknown;
  surpassed_threshold?: unknown;
  /// Read only to separate a refusal from paid overage, which Claude reports
  /// under the same `status`. Part of this window's own context for the same
  /// reason the other three are.
  is_using_overage?: unknown;
  /// When the delivering reading was observed, ISO-8601. **Absent for a window
  /// restored from a file written before instants were held per window**, which
  /// renders with no age line rather than borrowing the reading-level instant —
  /// that would date this window by when a *different* one was measured.
  observed_at?: string;
  /// Model of the delivering turn. The model-gated weekly window carries no name
  /// of its own, so this is the only thing that can label it.
  model?: string;
  /// The turn whose event contributed this window.
  ///
  /// **Scopes the late label fill to the turn that measured it, and the turn is
  /// the right grain — the agent is not.** Two hazards, and agent identity only
  /// covers the first. Two agents interleaving across one turn's
  /// rate-limit-event/`init` boundary would let the second agent's model name the
  /// first agent's window; and a turn that dies before reporting its model leaves
  /// a blank that the *same* agent's next turn would fill with a different model.
  /// `runtimeReducer` already refuses the second hazard one layer down, clearing
  /// the per-turn model at every turn start so a dead turn cannot lend its model
  /// to the next turn's reading.
  ///
  /// Under merging a mislabel outlives the reading that caused it, rendering until
  /// the window resets — and the correction path is closed by the same condition
  /// that creates the state, since only another turn on the gated model can fix
  /// the label and the user is capped on that model.
  ///
  /// **Deliberately not restored from the file.** The fill is a within-turn
  /// repair, so a persisted eligibility could only ever authorize a stale one; see
  /// `asStoredWindows`.
  turn_id?: TurnId;
};

/// Whether this harness's readings are **partial**, so each one contributes
/// windows to a retained set rather than replacing it.
///
/// The governing questions are whether a reading is *complete*, and whether its
/// parts are identifiable from the reading itself:
///
/// | Reading | Complete? | Identity carried in the reading? | Correct policy |
/// | --- | --- | --- | --- |
/// | Codex `account/rateLimits/read` | yes | yes — `limit_id` | replace; nothing is missing |
/// | Claude `rate_limit_event` | **no** | yes — the key *is* the identity | **merge per window** |
/// | Codex rollout `rate_limits` | no | **no** | neither; the source was replaced |
///
/// Claude's `unifiedWindows` omits the model-gated weekly window unless the turn
/// ran on one of the models it gates, so an Opus turn's reading is a complete
/// statement about the windows it names and silent about the rest. Replacing on
/// it deleted a cap that was still blocking work, which is the defect this exists
/// to fix.
///
/// **The third row fails the first column regardless of how the second resolves,
/// and that is the argument to lean on**: one bucket of N can never be a complete
/// reading. The second column says "carried in the reading" rather than
/// "knowable" deliberately — a rollout's neighbouring records *do* co-vary with
/// the bucket, so a future reader checking this table against one would find a
/// model slug there and conclude the table is wrong. The honest claim is that
/// identity would have to be reconstructed from an adjacent record by a rule
/// nothing establishes.
///
/// **The first row's premise is the one with no detection if it is wrong.** It
/// rests on the schema's own wording plus a single capture on a single plan, and
/// `replace` silently drops a bucket that stops appearing.
///
/// This table lives in the code, not only in a plan, because it is what stops a
/// future reader making one harness match the other.
export function reportsPartialUsageWindows(harness: HarnessKind | undefined): boolean {
  return harness === "claude_code";
}

/// Lift the windows out of a Claude reading, each tagged with what delivered it.
///
/// Iterates `CLAUDE_WINDOWS` rather than the payload's keys, so the store holds
/// only windows this file can name — the same rule that keeps unlabelled keys
/// from rendering keeps them from being persisted.
///
/// Returns `undefined` rather than an empty map when the reading carries no
/// recognised window, so a harness with no `unifiedWindows` at all contributes
/// no window map instead of an empty one. That is what lets the store decide
/// merge-versus-replace from the reading's shape alone.
export function claudeStoredWindows(
  payload: unknown,
  context: { observedAt?: string; model?: string; turnId?: TurnId },
): Record<string, StoredUsageWindow> | undefined {
  if (typeof payload !== "object" || payload === null) return undefined;
  const p = payload as {
    status?: unknown;
    rateLimitType?: unknown;
    surpassedThreshold?: unknown;
    unifiedWindows?: unknown;
  };
  const unified = p.unifiedWindows;
  if (typeof unified !== "object" || unified === null) return undefined;
  const windows: Record<string, StoredUsageWindow> = {};
  for (const { key } of CLAUDE_WINDOWS) {
    const w = (unified as Record<string, unknown>)[key];
    if (typeof w !== "object" || w === null) continue;
    windows[key] = {
      window: w,
      status: p.status,
      rate_limit_type: p.rateLimitType,
      surpassed_threshold: p.surpassedThreshold,
      is_using_overage: (payload as { isUsingOverage?: unknown }).isUsingOverage,
      observed_at: context.observedAt,
      model: context.model,
      turn_id: context.turnId,
    };
  }
  return Object.keys(windows).length > 0 ? windows : undefined;
}

/// The reset a stored window states, in vendor seconds, or `null` when it cannot
/// be read.
///
/// Exported because the store orders two entries for one key by it, and a reset is
/// the vendor's own statement of *which generation* of the window this is. The read
/// lives here with the rest of the payload knowledge; the ordering rule it feeds
/// lives beside `isNewer`, in the module that owns which reading wins.
///
/// **`resetsAt` is a same-key comparison, not part of the identity.** Keying
/// storage by `(key, resetsAt)` would imply two live entries for one window and
/// require something to arbitrate between them, which is worse than what it fixes.
export function windowResetsAt(stored: StoredUsageWindow): number | null {
  const w = stored.window;
  if (typeof w !== "object" || w === null) return null;
  const resetsAt = (w as { resetsAt?: unknown }).resetsAt;
  return typeof resetsAt === "number" ? resetsAt : null;
}

/// Defensive read of Claude's retained windows plus the newest reading's
/// account-level fields.
///
/// **Windows come from `stored`, not from `payload`.** A Claude reading names
/// only the windows the turn's model touched, so the displayed set is the union
/// the store has retained; `payload` supplies only what genuinely belongs to the
/// newest reading — the overage escalation and the no-window-map fallback.
///
/// Each window's tone is derived from the context of the reading that delivered
/// *that window*, so a later turn cannot clear a refusal it never measured, and a
/// threshold flag the newest reading drops is cleared on the window it named.
///
/// The top-level `resetsAt` / `rateLimitType` pair is a **fallback only**, for an
/// older CLI (or a future one that drops `unifiedWindows`): it has no percentage,
/// so it renders as a bare reset line rather than a meter. A bar with no value
/// would be a blank bar, which the card's clean-hide convention forbids more than
/// it forbids a missing bar. The field is undocumented and could vanish without
/// notice, so this is not dead code.
///
/// Every window is gated on its own reset being in the *future*. A reset is an
/// absolute timestamp, so it stays accurate however old the reading is — right
/// until `nowMs` passes it, at which point that window has cycled, we do not have
/// its new reset, and it drops while its siblings stay. That is also what retires
/// a retained window's flags, so no age threshold is needed.
///
/// `overage` is the separate "using credits" escalation (`isUsingOverage`), about
/// what is being *billed* rather than how full a window is. Its own window can be
/// days out, so it lives in the tooltip. A null overage reset ("flag set, no
/// window time") is still shown — we can't prove it stale. **Taken from the newest
/// reading, which assumes it describes the account rather than the window that
/// triggered it**; unprobeable on the development account and recorded in the gap
/// register with what would close it.
///
/// Returns `null` when nothing is displayable.
export function claudeRateLimitView(
  payload: unknown,
  stored: Record<string, StoredUsageWindow> | undefined,
  nowMs: number,
): {
  windows: UsageWindow[];
  fallback: { label: string; resetsAtMs: number } | null;
  overage: { resetsAtMs: number | null } | null;
} | null {
  if (typeof payload !== "object" || payload === null) return null;
  const p = payload as {
    rateLimitType?: unknown;
    resetsAt?: unknown;
    isUsingOverage?: unknown;
    overageResetsAt?: unknown;
    unifiedWindows?: unknown;
  };

  const windows: UsageWindow[] = [];
  for (const { key, label } of CLAUDE_WINDOWS) {
    const held = stored?.[key];
    if (held === undefined) continue;
    const w = held.window;
    if (typeof w !== "object" || w === null) continue;
    const ww = w as { utilization?: unknown; resetsAt?: unknown };
    if (typeof ww.utilization !== "number") continue;
    if (!(ww.utilization >= 0 && ww.utilization <= 1)) continue;
    if (typeof ww.resetsAt !== "number") continue;
    const resetsAtMs = ww.resetsAt * 1000;
    if (resetsAtMs <= nowMs) continue;
    // The threshold flag names its window in `rateLimitType` and its level in
    // `surpassedThreshold`, so a reading flags at most one of the windows it
    // delivered. A flag naming a window outside `CLAUDE_WINDOWS` is dropped with
    // that window, losing the signal — unobserved, and deliberately not
    // backfilled with a generic amber line.
    const flagged = held.status === "allowed_warning" && held.rate_limit_type === key;
    // **The wall, which is a different status from the warning.** At a warning
    // Claude sends `allowed_warning` plus a numeric `surpassedThreshold`; at a
    // refusal it sends `rejected` and no threshold at all, so the warning path
    // above leaves every window unflagged and the meter would draw a spent quota
    // in the neutral tone. `rateLimitType` names the window that did the blocking
    // in both cases.
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
    // recorded a windowless payload on a refused turn, so its last number was
    // stale and the refusal was the only truthful thing left; Claude reports the
    // blocked window's own utilization in the same payload that refuses, so the
    // number is already right and only the tone was missing.
    const refused =
      held.status === "rejected" && held.is_using_overage !== true && held.rate_limit_type === key;
    windows.push({
      key,
      label: label ?? modelWeeklyLabel(held.model),
      usedFraction: ww.utilization,
      resetsAtMs,
      measuredAt: held.observed_at,
      surpassedThreshold:
        flagged && typeof held.surpassed_threshold === "number"
          ? held.surpassed_threshold
          : undefined,
      limitReached: refused ? true : undefined,
    });
  }

  // **Two conditions, and they answer different questions.** `unifiedWindows`
  // being absent or empty is what makes the top-level pair the best signal the
  // *newest reading* has: a non-empty container whose entries were all filtered
  // out (reset-passed, unreadable, or a key we exclude) stays authoritative, and
  // falling back there would override the per-window rules rather than fill a
  // gap. Merging adds the second condition — a retained window still on screen
  // suppresses the line even when the newest reading carries no map at all,
  // because a bare reset date beside live meters reads as a sixth window.
  const unified = p.unifiedWindows;
  const hasUnified =
    typeof unified === "object" && unified !== null && Object.keys(unified).length > 0;
  let fallback: { label: string; resetsAtMs: number } | null = null;
  if (!hasUnified && windows.length === 0 && typeof p.resetsAt === "number") {
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

/// What a Codex account payload could not be read as, for the ingestion log.
///
/// **Returned rather than logged here**, because this function is pure and is
/// called from a `$derived` — a warning inside it would re-fire on every
/// recompute. The read path logs these once per distinct condition.
///
/// This exists for one reason: the quota filter below is the only thing
/// standing between a valid-but-changed Codex response and a panel that
/// silently renders nothing. Every field on a quota is optional in the
/// protocol, so a server that stops emitting nulls empties the section without
/// failing anything.
///
/// **Every variant is collected before any time-dependent filtering**, so the
/// ingestion call and the render call always agree whatever instant each
/// passes. That is what makes it safe to run this reader twice per read — once
/// to log, once to draw — rather than maintaining a second walker that would
/// drift from this one. A variant added below the reset check would break it
/// silently; a test asserts identical diagnostics at two far-apart instants.
export type CodexAccountDiagnostic =
  /// A quota carried no `normalModelSlug` key at all, so it could not be
  /// classified as account-wide or model-scoped and was skipped.
  | { kind: "bucket-without-model-association"; limitId: string }
  /// A quota declared a window whose `usedPercent` is missing or not a number.
  /// Carries the slot, because a quota has two and the useful thing to know is
  /// which one could not be read.
  | { kind: "window-without-usable-percent"; limitId: string; slot: string };

/// Codex's **account quota** payload, read into the bars the meter draws.
///
/// The payload is the response of `account/rateLimits/read`, stored under its
/// own wrapping as `{ordinaryUsageAllowed, rateLimitsByLimitId}`.
///
/// **This replaced a reader that could not tell which quota it held.** Codex's
/// per-turn rollout reports one quota under identical identifiers whichever
/// limit it describes, so the old path had to assert a scope it could not check
/// and paint a guessed window full on a refusal. Reading the account is what
/// makes the scope knowable: each quota declares its model association.
///
/// **A quota is a container of up to two windows, not a window.** Each carries
/// a `primary` and a `secondary` slot — typically a short window and a weekly
/// one for the *same* limit — and both are rendered, keyed and expired
/// independently. Reading only `primary` drops the second quota entirely and
/// takes the whole row down when the first slot cycles, which is a shape this
/// account's plan cannot produce and other plans can.
///
/// **Which quotas render: the account-wide ones, identified by having no
/// associated model.** A quota carrying a `normalModelSlug` is a model-specific
/// reserve (the observed one is `gpt-reserve` on `gpt-5.6-luna`), and showing it
/// beside an account allowance invites reading a reserve's headroom as the quota
/// that governs ordinary work.
///
/// Filtering on the model association rather than on `limitId === "codex"` is
/// deliberate: the identifier is not guaranteed across plans, and this rule
/// keeps working on a plan carrying both a 5-hour and a weekly account limit —
/// which is how Claude's section already behaves.
///
/// **A quota missing `normalModelSlug` entirely is skipped, not treated as
/// account-wide**, and reported. Treating absence as "no model" would promote
/// every reserve into the account section and label it as an ordinary
/// allowance — the defect this milestone removes, rebuilt in a new place.
///
/// **`rateLimitReachedType` is deliberately not read.** Codex reports a reason
/// code alongside each quota, and four of its five values are team/business
/// billing states (credits depleted, workspace spend caps) that say nothing
/// about a usage window. Deciding which bar a reason code applied to, and
/// whether it should become an account-wide message, was machinery for account
/// types this product does not target and cannot test against — and it produced
/// a defect in two consecutive review rounds.
///
/// **What the meter claims, precisely: it warns when measured usage reaches
/// 100%. It does not determine whether requests are permitted.** A refusal below
/// 100%, if any plan produces one, renders neutral — that is the accepted cost
/// of reading one number instead of a five-valued enum. The supporting evidence
/// is that `usedPercent` is reliably populated when a quota is spent, across
/// eight separate exhaustions between 2026-05 and 2026-09 in the local rollout
/// corpus; it is *not* evidence that the account read's reason code is
/// unreliable, since the rollout never emits that field at all.
///
/// `spendControlReached` and `individualLimit` arrive in the same snapshot and
/// are also not read. They were weighed rather than overlooked: the captured
/// account reports `spendControlReached: false` with `individualLimit` null, and
/// neither field's meaning is established well enough to render from — the name
/// of the second suggests an individual cap but nothing confirms it.
export function codexAccountUsageView(
  payload: unknown,
  nowMs: number,
): { windows: UsageWindow[]; diagnostics: CodexAccountDiagnostic[] } {
  if (typeof payload !== "object" || payload === null) return { windows: [], diagnostics: [] };
  const buckets = (payload as { rateLimitsByLimitId?: unknown }).rateLimitsByLimitId;
  if (typeof buckets !== "object" || buckets === null) return { windows: [], diagnostics: [] };

  const windows: UsageWindow[] = [];
  const diagnostics: CodexAccountDiagnostic[] = [];
  for (const [limitId, bucket] of Object.entries(buckets as Record<string, unknown>)) {
    if (typeof bucket !== "object" || bucket === null) continue;
    const b = bucket as {
      limitName?: unknown;
      normalModelSlug?: unknown;
      primary?: unknown;
      secondary?: unknown;
    };
    if (!("normalModelSlug" in b)) {
      diagnostics.push({ kind: "bucket-without-model-association", limitId });
      continue;
    }
    if (b.normalModelSlug !== null) continue;

    for (const slot of ["primary", "secondary"] as const) {
      const w = b[slot];
      // A windowless quota is a shape Codex emits (`limit_id: "premium"` arrives
      // with both slots null on a refused turn); it is skipped rather than
      // rendered as a bar with no value.
      if (typeof w !== "object" || w === null) continue;
      const ww = w as { usedPercent?: unknown; resetsAt?: unknown; windowDurationMins?: unknown };
      if (typeof ww.usedPercent !== "number") {
        diagnostics.push({ kind: "window-without-usable-percent", limitId, slot });
        continue;
      }
      let resetsAtMs: number | null = null;
      if (typeof ww.resetsAt === "number") {
        const ms = ww.resetsAt * 1000;
        // Reset passed → the window cycled and this percentage describes the
        // window before it, so it drops while its siblings stay.
        if (ms <= nowMs) continue;
        resetsAtMs = ms;
      }
      windows.push({
        // Keyed by quota *and* slot: one quota contributes up to two rows, and
        // they must not collide.
        key: `${limitId}:${slot}`,
        label: codexBucketLabel(ww.windowDurationMins, b.limitName),
        usedFraction: ww.usedPercent / 100,
        resetsAtMs,
        // **Decided here rather than in the meter template**, which draws every
        // harness: a "full bar is a warning" rule written there also caught
        // Claude, whose reader deliberately leaves a spent window neutral while
        // the user is paying for overage and requests still succeed.
        limitReached: ww.usedPercent >= 100 ? true : undefined,
      });
    }
  }
  return { windows, diagnostics };
}

/// Name an **account-wide** quota's window, using the shared cross-harness
/// vocabulary.
///
/// **This is the same string Claude's `seven_day` row uses, and that is the
/// point.** A user reading the two harnesses' rows side by side should be
/// comparing quantities, not decoding two vocabularies for one idea.
///
/// **Why "all models" is a reading of the data here and was an invention
/// before.** The old rollout path received a single unnamed quota and could not
/// tell the account allowance from the model reserve, so calling it "all models"
/// asserted something that might have been false — that was the original defect.
/// This reader only ever labels quotas that passed the `normalModelSlug === null`
/// filter, so the quota in hand is *by construction* the one with no model
/// association. The duration is likewise stated by the payload
/// (`windowDurationMins`), not guessed.
///
/// Falls back to Codex's own `limitName` for a duration we do not recognize —
/// the account-wide quota carries `limitName: null` in every capture, so this is
/// for a future shape rather than today's — and to a neutral noun when there is
/// nothing to go on. What it never does is invent a duration or a scope.
///
/// **Two account-wide windows sharing a duration would share a label.** They
/// stay distinguishable by key and reset time, and no supported plan produces
/// the shape, so nothing disambiguates them yet; a quota's two windows are
/// separated by their durations, which is what the slots are for.
function codexBucketLabel(windowDurationMins: unknown, limitName: unknown): string {
  if (windowDurationMins === 10080) return LABEL_WEEKLY_ALL;
  if (windowDurationMins === 300) return LABEL_FIVE_HOUR;
  return typeof limitName === "string" && limitName !== "" ? limitName : "Quota";
}
