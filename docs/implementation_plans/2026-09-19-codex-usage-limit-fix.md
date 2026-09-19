# Codex usage limits: read the account, stop inferring it

**Status:** planned, not started. Revised 2026-09-19 after two review rounds and a live probe.
**Depends on:** PR #105 (`agent-card-metadata`), landed as `9c544df`. This plan deletes some of what
that PR added; see M3.
**Codex CLI at time of writing:** 0.154.0.

## The problem

Switchboard's Codex quota meter showed **"Weekly · all models — in 7 d — 100%"** while the user was
actually blocked by a *different* quota resetting in three hours. Codex's own `/status` reported
both correctly:

```
Weekly limit:                [░░░░░░░░░░░░░░░░░░░░] 0% left (resets 12:18)
Luna Reserve Weekly limit:   [███████████████████░] 95% left (resets 15:31 on 25 Sep)
```

Every part of our row was wrong except the number, and the number was right by accident.

### Root cause

**An account can hold several metered quotas at once** — here a general weekly limit and a
model-specific reserve. The rollout file we read (`~/.codex/sessions/.../rollout-*.jsonl`) records
**one** of them per turn, in a `primary`/`secondary` pair:

```json
{"type":"token_count","rate_limits":{
  "limit_id":"codex","limit_name":null,
  "primary":{"used_percent":5.0,"window_minutes":10080,"resets_at":1790375461},
  "secondary":null,"plan_type":"prolite"}}
```

**Critically, the rollout cannot distinguish the two.** The reserve above arrives carrying
`limit_id: "codex"` and `limit_name: null` — byte-identical identifiers to the general weekly
quota. Only the reset timestamp differs, and which quota lands in the slot **alternates between
turns**. Two Switchboard installs on the same machine held different quotas at the same moment
purely because they had observed different turns.

Three defects follow, all consequences of reading an unnamed single bucket:

1. **Invented label.** `usageWindows.ts::codexWindowLabel` maps `window_minutes === 10080` to
   `"Weekly · all models"`. That claim is ours, not Codex's, and it was false — the bucket was a
   model reserve.
2. **Silent overwrite.** The store keeps the newest whole snapshot, so a reading of quota B erases
   quota A. The exhausted quota simply vanished.
3. **Misattributed refusal.** Because the rollout never says *which* quota refused,
   `codexRateLimitView` picks the most-used window and forces it to 100%. Holding only the 5%
   reserve, it painted 100% onto the reserve and kept the reserve's reset date.

### The information exists; we were reading the wrong surface

The generated app-server schema labels the field we read as legacy:

```
rateLimits:          "Backward-compatible single-bucket view; mirrors the historical payload."
rateLimitsByLimitId: "Multi-bucket view keyed by metered limit_id (for example, `codex`)."
```

`account/rateLimits/read` returns, verbatim:

```json
{"ordinaryUsageAllowed": false,
 "rateLimitsByLimitId": {
   "codex": {"limitName": null, "normalModelSlug": null,
             "primary": {"usedPercent": 100, "windowDurationMins": 10080, "resetsAt": 1789845487},
             "rateLimitReachedType": "rate_limit_reached"},
   "base_model_inference": {"limitName": "gpt-reserve", "normalModelSlug": "gpt-5.6-luna",
             "primary": {"usedPercent": 5, "windowDurationMins": 10080, "resetsAt": 1790375461},
             "rateLimitReachedType": null}},
 "rateLimitResetCredits": {...}, "rateLimitUpsell": {...}, "accountId": "..."}
```

Measured properties of the call, cold spawn, no daemon:

- **0.5–0.9s** end to end including process start.
- Costs **no quota**, requires **no model call**.
- **Works while rate-limited** — this is how the data was captured.
- The `initialized` notification is **not** required.
- Leaves **no orphan process** when the child is killed.

### Product decision: show only account-wide quotas

**The model reserve is not rendered.** The user's stated need is to know when their ordinary weekly
allowance is spent. The reserve is a fallback they do not use on an ongoing basis; when capped
mid-task they will finish it in the Codex terminal and wait for the reset. Surfacing a second bar
for a pool they do not plan to spend adds noise to the one number they care about.

This also removes a rendering problem rather than creating one. An earlier revision of this plan
carried a three-way state vocabulary (bucket *exhausted* / account *degraded* / turn *refused*)
because a spent general quota sitting beside a healthy reserve made "blocked" a false description of
the account. With the reserve hidden there is no sibling to contradict, so the bar states that the
weekly allowance is spent, which is true and complete.

**`GPT-Reserve` is not added to the model picker.** See the probe results below — it is a real,
dispatchable catalog model, so this is a decision rather than a limitation. The terminal is the
documented answer for finishing a task while capped, and M4 records that for users.

### Live probe results (2026-09-19, ordinary quota spent)

Run against the degraded account to settle a hypothesis raised in review — that pinning `-m` might
defeat an automatic fallback to the reserve, making this a dispatch bug rather than a display bug:

| Dispatch | Result |
| --- | --- |
| `codex exec` **unpinned** | `usage_limit_exceeded` — refused |
| `-m gpt-5.6-luna` (the `normalModelSlug`) | `usage_limit_exceeded` — refused |
| `-m gpt-reserve` (the `limitName`) | **succeeded** |

`codex debug models` lists `gpt-reserve` as a first-class catalog entry with display name
`GPT-Reserve`.

**Conclusions, all load-bearing for this plan:**

- **There is no dispatch bug.** Unpinning does not reach the reserve; the hypothesis is refuted and
  no change to `build_args` follows.
- **`normalModelSlug` is a display alias, not a dispatchable slug** — consistent with the schema
  ("Normal model whose display name and reasoning options describe this quota alias"). `limitName`
  happens to be the dispatchable one here. Do not assume either field is a model you can send.
- **A model-scoped quota is identifiable by `normalModelSlug` being non-null.** This is the rule M2
  filters on.

### Verification already done — do not re-probe (it costs quota and a wall to reproduce)

- No rollout record carries both quotas. Across all recent rollouts the `rate_limits` object has
  exactly one key set, containing only `primary`/`secondary` and no field capable of holding named
  buckets. Multi-bucket field names appear in no rollout; two apparent grep hits were a user message
  quoting this analysis and a `gpt-reserve` **model slug** in `turn_context`.
- **The rollout is not entirely unlabelled, and the earlier draft of this plan overstated that.**
  `turn_context.model` co-varies with the bucket at the one observed transition (model flips to
  `gpt-reserve` 3.4s before the reserve bucket first appears), and that slug matches the API's
  `limitName` exactly. A deterministic rollout-only rule is still **not** established —
  `gpt-5.6-luna` appears against both a reserve bucket and a windowless `premium` record — so the
  rollout remains unusable as a source, but for the accurate reason: it can only ever carry one
  bucket of N, and one-of-N is what made the meter wrong regardless of naming.
- Claude has no equivalent command (`claude --help` has no usage/status subcommand) and needs none —
  its `rate_limit_event` already names every window in one payload.
- A Claude `/context` run does **not** emit a `rate_limit_event`, so it is not a free refresh path
  (checked against `crates/harness/tests/fixtures/claude/context-report.stream.jsonl`).

## Reading required before implementing

- **The generated protocol schema is the contract.** Run
  `codex app-server generate-json-schema --out <dir>` and read `GetAccountRateLimitsResponse`,
  `RateLimitSnapshot`, `RateLimitWindow`. This is not published documentation; the generated schema
  ships with the installed CLI and is the authority.
- `docs/harness-behavior.md` — the repo's single source of truth for harness behavior. §1.4, §2's
  kind table (`:96`), G3 (`:573`), G7, G8, and the §5 entry at `:662` all touch this work.
- `AGENTS.md` — live-test policy and naming, `make` target rules, foreground-execution rule.
- https://developers.openai.com/codex/ — general Codex CLI docs. The app-server protocol is **not**
  documented there.

## Approach, and what it was chosen over

**Chosen:** call `account/rateLimits/read`, render the account-wide quotas it returns.

**Rejected — key the existing rollout readings by reset time.** Works without new plumbing, but can
never name a quota or say which is blocking, so the meter would show two identically-labelled
"Weekly" rows distinguished only by date. Treats the symptom.

**Rejected — keep the rollout path as an offline fallback.** A fallback that can only ever carry one
bucket of N cannot show the account-wide quota when the reserve is the one reported — and with the
reserve now hidden, such a reading would render *nothing* while looking like a working path.
Restart continuity is already covered by the persisted store.

**Deliberately not built:** a manual refresh button. The call is sub-second and free, so the app
keeps itself current rather than asking the user to press something. Matches the convention in
`ContextBreakdown.svelte:10` ("opening is the refresh"). Do not add one.

### Non-goals

- **Claude is untouched.** Its stream payload is already complete. The asymmetry is intentional.
- **The model reserve is not rendered**, and `GPT-Reserve` is not added to the picker. Decided, not
  deferred.
- **`rateLimitUpsell` is not rendered.** It is entirely about the reserve; it is `dismissible` with
  `ctas` including "Add Credits", making it a vendor marketing surface; and its `description`
  carries a `{time}` placeholder, so rendering it means maintaining a template renderer for copy
  that can change without a version bump and would have no drift test. Its shape is recorded in M4.
- **`rateLimitResetCredits` is not rendered.** The account currently holds an unconsumed free reset;
  spending one is a real action with real consequences and deserves its own decision. Shape recorded
  in M4.
- **No periodic polling timer.**

### Conventions this work establishes, for reuse by later milestones

1. **An account-scoped harness read** — a harness call belonging to no agent and no project. New;
   every Codex subprocess today is an agent turn. It must not touch the `HarnessAdapter` trait, the
   dispatcher, the per-agent FIFO, session locks, or the journal. M1 establishes where this lives;
   later milestones reuse it rather than inventing a second path.
2. **Rust does not interpret the payload.** `src/lib/usageWindows.ts` remains the single interpreting
   layer. The command extracts the JSON-RPC `result` and passes the relevant subtree through verbatim.
3. **Degrade to the last known reading, never to an error.** Every failure mode (spawn failure,
   timeout, logged out, malformed response) leaves the held reading in place and logs. It must never
   fail a turn or block the UI.

---

## M1 — Account usage read (backend)

### Goal & Outcome

Give the app a way to ask Codex for the account's complete, named quota state.

- Switchboard can retrieve every metered quota for the logged-in Codex account, each with its name,
  model association (if any), used percentage, window duration, reset time, and whether that quota
  is exhausted — plus the account-level flag for whether ordinary usage is permitted.
- The call completes in about a second, costs no quota, and succeeds while the account is capped.
- Any failure reports "no reading available" rather than an error the user must act on.
- A Claude-only user never pays for a Codex spawn.
- A live test fails on a developer's machine if OpenAI changes the shape we depend on.

### Implementation Outline

**Where it lives.** `crates/harness`, beside the Codex adapter. The nearest precedent,
`check_codex_auth_impl` (`crates/app/src/commands.rs:6916`), sits in `crates/app`, but that is a
filesystem existence check with no protocol knowledge. This speaks Codex's own JSON-RPC and belongs
with the code that knows Codex's wire formats. A free function, not a `HarnessAdapter` method — the
trait is per-turn and this is not a turn.

**The call sequence**, established by probe:

1. Spawn the resolved `codex` binary with `app-server`, piped stdin/stdout.
2. Write `{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"clientInfo":{...}}}`.
3. Write `{"jsonrpc":"2.0","id":1,"method":"account/rateLimits/read","params":{}}`.
4. Read stdout until the object with `"id":1` arrives. **Other traffic is interleaved** — the probe
   saw an unsolicited `remoteControl/status/changed` notification between the two responses — so
   match on id and skip everything else rather than assuming line position.
5. Kill the child. It holds stdio open indefinitely otherwise.

Reuse `resolve_binary`, `apply_path_env`, `map_spawn_error` from
`crates/harness/src/subprocess.rs`; `fetch_version` there is the precedent for one-shot
spawn-and-parse.

**Contract.** Return `rateLimitsByLimitId` and `ordinaryUsageAllowed` as opaque JSON per convention
2. Rust confirms a JSON-RPC success arrived, extracts those two fields, and hands them over. No
window interpretation, labelling, or exhaustion detection in Rust.

**Constraints that are load-bearing:**

- **A timeout is required**, and the child must be killed on *every* exit path including timeout and
  parse failure. A leaked `codex app-server` holding stdio is the failure mode to avoid.
- **Do not require the `initialized` notification** — probed, the call succeeds without it.
- **Do not start or depend on the app-server daemon.** A standalone stdio spawn is stateless and
  works.
- **Do not hardcode the observed `limit_id` values** (`codex`, `base_model_inference`). Iterate
  whatever the map contains; filtering happens in M2.

**Capability gating.** Follow the predicate pattern in `crates/core/src/harness.rs`
(`supports_context_report` and siblings) so callers ask the capability rather than matching on
harness kind. Only Codex has this.

**Spawn gating.** The startup call must not run for users who never use Codex — a Claude-only launch
would pay a failed spawn and a warning for a meter that can never render. Gate on evidence Codex is
in use: an existing `codex` entry in `usage.yaml`, a configured Codex agent, or the auth-file check
`check_codex_auth_impl` already performs. The turn-end trigger needs no gate; a Codex turn is itself
the evidence.

**The one unprobed behavior behind M2's rendering rule.** Every capture to date has
`ordinaryUsageAllowed: false`. The question that matters is narrow, and it is **not** "record the
healthy shape" generally:

> Does a **refreshed** bucket — new `resetsAt` in the future, `usedPercent: 0` — still carry a
> leftover `rateLimitReachedType`?

A window-scoped exhaustion cannot go stale *within* its window (capped stays capped until reset),
and once the reset passes M2's reset-passed rule drops the bucket along with its flag. The refreshed
bucket is the only residual.

**Do not wait for this probe to implement.** M2 ships the assumed-safe rule stated there, and the
probe runs as a pre-PR gate (see "Pre-PR verification" below) to confirm or replace it.

Other unprobed items — logged-out and offline responses, bucket sets on plans other than `prolite` —
are handled as generic failures and recorded in M4 rather than designed for.

### Definition of Done

- Unit tests over the response-parsing seam using recorded fixtures: the real captured response; an
  absent or empty `rateLimitsByLimitId`; a JSON-RPC error result; malformed JSON; and a stream where
  an unrelated notification precedes the matching id.
- A test proving the child is not left running after a timeout.
- **Both process-level tests run hermetically** via an `app-server` mode on the existing
  `crates/harness/src/bin/fake_codex.rs`. A timeout test cannot be made deterministic in the live
  suite, so it must not drift there.
- A live test named per the `live_codex_*` convention (required — `make test-live-codex` filters on
  that prefix). Assert the *structure* we depend on: buckets keyed by `limit_id`, each carrying a
  window with a used percentage and reset, `normalModelSlug` present as a field, and
  `ordinaryUsageAllowed` present. This is the drift detector for an experimental, undocumented
  protocol and is the primary mitigation for that risk.
- Rationale recorded in module documentation, not only here: why this lives in `crates/harness`, and
  the accepted exposure to an experimental protocol.

---

## M2 — Render account-wide quotas, delete the inference

### Goal & Outcome

Make the meter show the user's ordinary weekly allowance, correctly.

- The Codex section shows the account's **account-wide** quotas — not whichever quota the last turn
  happened to report, and not model-specific reserves.
- Each row is labelled from Codex's own data, never from a label we invent.
- A spent quota is drawn as spent because Codex says it is exhausted, not because we guessed.
- The display refreshes on startup, after each Codex turn, and when the panel becomes visible —
  without the user acting and without blocking anything.
- The meters never blank as a side effect of this change landing.

### Implementation Outline

**Ingestion cut happens here, not in M3.** This is load-bearing sequencing. `recordAccountUsage`
(`src/lib/state/index.svelte.ts:982`) routes every Codex `rate_limit_event` into `observeUsage`
stamped with **arrival time**, so it wins newest-wins over the account read every time;
`recordRestoredUsage` (`:429`) does the same on project open; `codex/mod.rs:821` still emits after
each `TurnEnd` until M3. If the frontend cut waits for M3, the Codex section clean-hides after every
turn and every project open for the whole duration of M2 — worse than the bug being fixed. **Both
frontend entry points stop routing Codex to the store as part of this milestone.**

**Call sites.** Startup (gated per M1), after each Codex turn ends, and on panel visibility.
Visibility matters for a specific reason: a blocked user's bucket renders fine until its reset
*passes*, and the section then empties at exactly the moment they are watching it — and they are not
ending turns, so nothing else would refresh. Not window focus, not a timer. This is the same
"opening is the refresh" rule cited above.

Fire-and-forget; never on a turn's critical path.

**Coalescing — drain semantics, not dedupe.** Mirror `harnessUsage.svelte.ts::persist` (`:222`)
*including* its follow-up: a request arriving while one is in flight sets a flag and triggers
exactly one further call. This is not a technicality — a read that started before a turn ended
returns a number predating that turn's consumption, so answering the later request from the in-flight
call would systematically understate usage.

**Which quotas render.** Show buckets whose `normalModelSlug` is null — account-wide allowances.
Hide model-scoped ones. **Filter on the model association, not on `limit_id === "codex"`**: the
identifier is not guaranteed across plans, and the rule as stated keeps working on a plan carrying
both a 5-hour and a weekly account limit, which is how Claude's section already behaves.

**Stored shape.** The Codex entry's `payload` holds the response's own wrapping —
`{ordinaryUsageAllowed, rateLimitsByLimitId}` — **not** a flattened bucket map. Two reasons, both
concrete:

- `asReading` (`harnessUsage.svelte.ts:184-200`) is a hard whitelist reconstructing exactly
  `payload`/`observed_at`/`model`/`limit_reached`. A sibling top-level `ordinary_usage_allowed`
  would be written by `persist` and silently dropped on load, leaving the account gate absent after
  every restart. Nesting it inside `payload` makes it ride the existing whitelist untouched.
- Flattening would require a Codex-specific shape sniff in a deliberately harness-agnostic loader,
  to drop old-shape entries. With the wrapping, an old-shape entry is structurally unreadable by the
  new view — it renders nothing and the next call supersedes it within a second. Flattening is also
  actively hazardous: the old payload's top-level keys (`primary`, `secondary`, `limit_id`,
  `plan_type`) would be iterated as if they were buckets, and a lenient label fallback could render
  a bucket named "primary".

Newest-wins by `observed_at` stays. `model` stays — Claude's model-gated weekly window needs it.

**Deletions.** These exist solely to support inference the account read replaces:

- `limit_reached` on the stored reading, and `recordUsageRefusal` / `clearUsageRefusal`.
- `sameCodexUsageWindows`.
- The `culprit.usedFraction = 1` painting in `codexRateLimitView`.
- The `"Weekly · all models"` literal. **Do not reintroduce a duration-derived label** — that
  invented claim is the first defect in this document. If a bucket cannot be named from the payload,
  say less, not more.

**Exhaustion — implement this rule, which rests on a stated assumption.**

> A bucket draws as exhausted when its `rateLimitReachedType` is non-null **and** its `usedPercent`
> is greater than zero.

`ordinaryUsageAllowed` is carried as data and may corroborate, but is **not** rendered as its own
line — with the reserve hidden there is no sibling for it to disambiguate, and the spent bar already
carries the message.

The `usedPercent > 0` conjunct is the guard for M1's unprobed refreshed-bucket question, and it is
deliberately *not* a threshold: a quota that is 0% spent and simultaneously exhausted is a
contradiction, so when the flag and the measurement disagree we trust the measurement and claim
nothing. No arbitrary percentage is invented, and the rule needs no revision if the probe comes back
clean.

Two things to be honest about in the code comment, not only here. This **infers recovery from a
percentage**, which the schema's own wording tells clients not to do ("must not infer recovery from
percentages or reset times") — accepted knowingly, as the narrowest available guard against a
failure mode we cannot yet rule out. And it errs toward **understating** a quota, which is the
direction this codebase takes throughout: a meter that fails to shout is recoverable, one that
falsely claims you are blocked is not.

**Staleness copy.** `HarnessUsage.svelte:214` reads "Measured {relativeTime} — **send a message to
refresh**." That instruction becomes false for Codex once the app refreshes itself, and the value's
meaning shifts too: for Codex `observed_at` stops meaning "when the harness measured it" (a rollout
stamp that could be days old, which is why the line exists) and becomes "when we last asked". Make
the line per-harness — Claude keeps the imperative, Codex states the measured instant alone.

**What stays.** `FailureKind::UsageLimit` remains, but **not** for the reason the previous revision
gave. `docs/harness-behavior.md:96` states that `error_kind` never changes how a failed turn renders
— every failed turn shows its verbatim message — so "the transcript should say so" was wrong. Keep
it on the honest ground: a typed classification that is expensive to re-derive and cheap to hold,
with no surface reading it after this milestone.

### Definition of Done

- Component tests: account-wide buckets render; a bucket with a non-null `normalModelSlug` does not;
  a bucket with a null name falls back without inventing one; an exhausted bucket renders spent and
  any sibling does not; an empty or absent map renders no Codex section rather than an empty one.
- **A bucket whose window is absent or unreadable is skipped**, and if that leaves none the section
  does not render. This shape is observed — `limit_id: "premium"` arrives with
  `{"primary":null,"secondary":null}` on a refused turn (see `usageWindows.ts:168-171`).
- A Codex `rate_limit_event` arriving after an account read does not replace it.
- An old-shape Codex entry in `usage.yaml` renders nothing and is superseded by the next read; a
  Claude entry beside it still loads.
- A burst of N concurrent refresh requests yields at most one in-flight call and at most one
  follow-up.
- Reset-passed behavior preserved: a bucket whose reset has elapsed drops while siblings stay.
- Grep confirms no `limit_reached` remains in the store, the view, or `usage.yaml`.

---

## M3 — Delete the Codex rollout rate-limit path

### Goal & Outcome

Remove the now-dead ingestion so there is exactly one source of Codex quota.

- Codex quota comes from one place; no second path can disagree with it.
- **No user-visible change** — M2 already cut the frontend over.

### Implementation Outline

Deletion only, ordered after M2 so the replacement is live and the frontend no longer consumes this
path.

Remove the Codex rate-limit reading from the session-file enrichment, the wire fields carrying it to
the frontend, and the `rate_limits_carry_window` capture guard, which existed only to stop a
windowless payload erasing a real one. The `rate_limits_observed_at` enrichment added by PR #105
goes with it — it ordered restored Codex readings against each other, and there are no longer
restored Codex readings.

**Two corrections to the previous revision's blast-radius note, both verified:**

- **`meta_sidecar.rs` needs no change.** It contains zero Codex references; rate-limit persistence
  there is gated on `RateLimitSource::StreamOnly` (`crates/dispatcher/src/lib.rs:2428`) and is
  deliberately harness-agnostic. The previous instruction to "remove only the Codex arms" described
  something that does not exist.
- **`RateLimitSource` stays intact.** Deleting the Codex emission (`codex/mod.rs:821`) leaves
  `SessionFileBacked` with no production producer, while its siblings `SessionMetaSource` and
  `ContextWindowSource` keep both variants because Codex still produces those. Keep the enum — it is
  `#[non_exhaustive]`, it encodes the class B/C distinction the docs lean on, and the three-way
  symmetry is documented at `events.rs:230-256`. Add a one-line note that its `SessionFileBacked`
  arm is knowingly producerless rather than leaving the next reader to find it and guess.

The Codex rollout reader does much more than rate limits (turn content, tool calls, inventory,
context window). Only the rate-limit extraction leaves.

### Definition of Done

- Rust and frontend suites pass. Tests that existed only to cover deleted behavior are deleted with
  it. (`AGENTS.md`: deleting a test *because its subject no longer exists* is correct; deleting one
  because it fails is not.)
- Claude's sidecar-backed rate-limit restore still works, proven by a test.
- `make test-live-codex` passes, covering the rollout reader's remaining responsibilities.

---

## M4 — Correct and close the harness record

### Goal & Outcome

The documented account of Codex quota behavior matches what is now known, and the captures made
during this investigation are preserved rather than summarized away.

### Implementation Outline

**`docs/harness-behavior.md:662` — "Codex coexisting weekly windows" — is in §5 "Open captures /
unverified" (§5 begins at `:659`), not the §4 gap register.** The previous revision of this plan
misidentified both the section and the error, and instructed a rewrite that would have destroyed an
accurate evidence trail. Reframe it as **closing an open capture**:

- **Keep** the observations — the 45%→100% climb, the 36-minute gap, the 05:48Z refusal naming the
  Sep 19 reset while disk held Sep 25. That timeline is the evidence trail for this investigation.
- **Replace the one false clause**: *"Merging window-by-window would be worse, not better (it would
  show both forever, and the account is in neither state), which is why the store takes whole
  snapshots."* The account is now known to be in both states at once. That sentence is what would
  stop a future reader from doing what this plan does.
- The entry's plan-change/credit-purchase line is already labelled "the assumption, not an
  observation" and needs correcting, not deleting: the second window is a model reserve.
- **Answer its two open questions.** What creates the second window: a model-scoped reserve quota,
  identifiable by `normalModelSlug`. Whether Codex ever reports both in one payload: yes, on a
  different surface — `account/rateLimits/read`.

**Also update, all verified as becoming false:**

- §2's kind table (`:96`) and **G3 (`:573`)**, which records "One narrow exception since:
  `usage_limit` is read by the sidebar to draw…" — M2 deletes that reader, so `error_kind` returns
  to having no readers at all.
- **G8**, which describes the rollout-derived Codex rendering M3 deletes.
- **§1.4** quick-reference.
- A version-log entry for the discovery, including the probe table (unpinned refused, `gpt-reserve`
  succeeded, `gpt-5.6-luna` refused) and the conclusion that `normalModelSlug` is a display alias
  rather than a dispatchable slug.

**Record in the gap register, because the captures are perishable and re-capturing needs another
wall:** the `rateLimitUpsell` shape (including `banner_type`, the `{time}` placeholder, `ctas`, and
`reset_at` pointing at the blocking quota), the `rateLimitResetCredits` shape, and M1's unprobed
items with what would close each.

**`README.md` "Harness support and limitations"** gains a short user-facing entry: when the Codex
weekly allowance is spent, Switchboard shows it as spent and the models in the picker will be
refused; a model reserve may still be available through the Codex terminal directly. Symptom first,
one or two plain lines — this is the documented answer for finishing a task while capped, and it
exists because we decided not to put `GPT-Reserve` in the picker.

### Definition of Done

- The §5 entry is closed with its observations intact and the false clause replaced.
- No remaining text describes the rollout as the Codex quota source.
- `:96`, G3, G8, §1.4 updated; version-log entry added with the probe table.
- Perishable captures and unprobed items recorded.

---

## Risks

1. **`codex app-server` is marked `[experimental]`.** Method names and response shapes can move
   without a version bump. The M1 live test is the mitigation and it is detection-after-the-fact —
   the same posture the repo takes for every undocumented field it reads. Accepted knowingly.
2. **No offline fallback.** A first run with no network shows no Codex meters. Accepted: the
   persisted store covers restarts, and a fallback that can only carry one bucket of N would — with
   the reserve hidden — often render nothing while appearing to work.
3. **A new spawn category.** No session-lock contention; it touches no session. It can spawn while
   turns run and shares PATH-resolution machinery. Bounded by M2's coalescing and M1's gating.
4. **The refreshed-bucket question ships unprobed.** M2's rule assumes a safe answer rather than
   waiting for one. The failure mode if the assumption is wrong is bounded and in the safe
   direction — an exhausted quota could render neutral rather than a healthy one rendering blocked.
   Closed by the pre-PR gate below.

---

## Pre-PR verification

**Run before opening the PR, not before implementing.** Both items need a real account state that
cannot be conjured on demand, which is why they gate the PR rather than the work.

1. **The refreshed-bucket probe.** Call `account/rateLimits/read` when a previously exhausted bucket
   has reset, and check whether `rateLimitReachedType` is still non-null on a bucket now reporting
   `usedPercent: 0` with a future `resetsAt`.

   - **Flag clears on reset** → the `usedPercent > 0` conjunct is redundant but harmless. Keep it
     (it costs nothing and guards a shape we have still only seen once), and record the result in
     the gap register so the next reader knows it was answered rather than assumed.
   - **Flag persists** → the conjunct is load-bearing. Record that plainly and add a fixture test
     pinning the refreshed-bucket shape, because at that point the flag alone is known-wrong and a
     future simplification would reintroduce the bug.

   Either way the answer replaces the assumption in M2's code comment with an observation.

2. **`make test-live-codex`.** Required by `AGENTS.md` before merging adapter-touching changes, and
   it needs quota to be available — the same reset that enables item 1.

Record both outcomes in M4's gap-register entries rather than leaving them in the PR description.
