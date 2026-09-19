# Usage limits: read the account, stop inferring it

Two defects with one root cause — **we assumed every quota reading tells the whole story, and
neither harness's does.** Codex reports one unnamed quota per turn, so we showed whichever one the
last turn mentioned (M1, M2, M4). Claude reports a complete-looking payload that silently omits the
model-gated weekly window unless that model ran, so a later turn deletes a cap that is still in force
(M3). The fixes differ because the data differs; M3 carries the table that explains why.

**Status:** planned, not started. Revised 2026-09-19 after two review rounds and a live probe.
**Depends on:** PR #105 (`agent-card-metadata`), landed as `9c544df`. This plan deletes some of what
that PR added; see M4.
**Codex CLI:** captured against 0.154.0; **revalidated against 0.155.1** on 2026-09-19 (schema,
wire shape, and every measured behavior below re-checked — see "Revalidation against 0.155.1").

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

- **Median 2.07s** end to end including process start; min 0.51s, p90 5.63s, max 8.28s over 25
  cold-spawn trials, measured **on a rate-limited account**. An earlier revision of this plan
  recorded "0.5–0.9s" from a two-sample probe — that was the fast tail, and it was wrong by roughly
  an order of magnitude at the other end. The call reaches OpenAI's backend, so it carries network
  latency. This is why `ACCOUNT_USAGE_TIMEOUT` is 30s rather than the 15s an earlier draft assumed,
  and why M2's coalescing is load-bearing rather than defensive: a multi-second read will routinely
  still be in flight when the next turn ends.

  **A re-measurement on a recovered account is far faster — and does not license lowering the
  bound.** 25 fresh cold-spawn trials on 0.155.1 with quota available: min 0.51s, median 0.59s, p90
  0.94s, max 1.28s. The fast end is identical to the capped run; the entire difference is the tail.
  Two variables changed between the runs (CLI version *and* account state), so the improvement
  cannot be attributed to either — but the hypothesis that fits the shape is that the slow tail
  belongs to the capped state, where the response additionally carries a populated `rateLimitUpsell`
  banner and the backend has reset-credit work to do. If that is what it is, **the slow case is
  exactly the case the meter exists for**, and sizing the timeout from the healthy distribution
  would make it fire precisely when a user is capped and looking. The bound stays 30s.
- Costs **no quota**, requires **no model call**.
- **Works while rate-limited** — this is how the data was captured.
- The `initialized` notification is **not** required.
- Leaves **no orphan process** when the child is killed.

### Revalidation against 0.155.1

The capture above was taken on 0.154.0. Codex moved to **0.155.1** and every assumption this plan
rests on was re-checked against it on 2026-09-19 rather than assumed to survive. Nothing broke.

**Protocol surface — unchanged.** Regenerated the schema (`codex app-server
generate-json-schema`). `initialize` and `account/rateLimits/read` both still exist among the 102
client request methods. `InitializeParams` still requires only `clientInfo` (with `capabilities`
optional), so the handshake we send still validates. On the response, `rateLimits` is still the sole
`required` property, `rateLimitsByLimitId` is still an optional *and* nullable map of `limit_id` →
snapshot, and `ordinaryUsageAllowed` is still a nullable boolean. `GetAccountRateLimitsParams` still
carries both `excludeResetCreditDetails` and `supportsLunaReserve` with unchanged meanings, so the
decision to set the first and not the second stands.

**Wire shape — identical key set.** The 0.155.1 response has the same top-level keys as the 0.154.0
capture (`accountId`, `ordinaryUsageAllowed`, `rateLimitResetCredits`, `rateLimitUpsell`,
`rateLimits`, `rateLimitsByLimitId`) and the same per-bucket keys, down to `credits`, `planType`,
`individualLimit`, and `spendControlReached`. The two captures differ **only** in account state. This
is worth stating precisely because it is the one thing a version bump could have broken silently:
the read stores buckets as opaque JSON, so a renamed field would not fail to parse — it would render
as "unknown" forever.

**Behavioral claims — re-measured, not re-asserted.** Each of these is a claim some comment in
`account_usage.rs` makes as a measured fact, so each was re-run:

- **Closing stdin still kills the read: 0/20 answered.** Holding it open: 25/25. The single most
  surprising property of this protocol is still true, and still undocumented.
- **The `initialized` notification is still not required** — the probe omits it and gets an answer.
- **The interleaved `remoteControl/status/changed` notification still arrives** between the handshake
  reply and the answer, so matching by JSON-RPC id rather than line position is still load-bearing.
- **Still works while rate-limited, and now confirmed to work while healthy.**
- **`codex app-server` still writes nothing to stderr on a clean read.**

**One shape worth flagging for M2, unchanged but easy to misread.** The account-wide `codex` bucket
carries `limitName: null` and `normalModelSlug: null`; only the reserve bucket
(`base_model_inference`) is named, as `"gpt-reserve"` / `"gpt-5.6-luna"`. The buckets are named by
their **map key**, not by `limitName`. M2's filter keys off `normalModelSlug` being null to mean
"account-wide", which is exactly right — but any UI tempted to *display* `limitName` would show
nothing for the one quota users actually care about.

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
answer for finishing a task while capped, and M5 records that for users.

**No explanatory sentence is added to the card either**, and the reason is evidence rather than
taste. Both reviewers argued for one, on the grounds that a bare 100% bar states a true-but-incomplete
story and leaves the user with no remedy. That argument is sound but its premise was wrong: the
interactive Codex CLI **already tells the user they are out and offers the reserve model**, confirmed
by the engineer doing exactly that and finishing the task. The explanation exists at the moment it
applies, from the vendor, with the switch attached. Duplicating it on the card would add a second
voice on a surface whose job is the number.

Note this also means the earlier probe result — that unpinned `codex exec` is refused — does **not**
generalise to the interactive CLI, which handles the fallback itself. Both surfaces the probe touched
were `codex_exec`; no interactive session exists in the corpus. Record that scope limit in M5 rather
than letting the probe table imply the terminal is a dead end.

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
- Claude has no equivalent command (`claude --help` has no usage/status subcommand). It needs none
  for *naming* — its windows are stably keyed — but **its readings are not complete**, which is M3:
  a payload omits the model-gated weekly window unless that turn ran on the gated model.
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

**Deliberately not built:** a manual refresh button. The call is free and fires on its own, so the
app keeps itself current rather than asking the user to press something. Note the reason is
*automatic*, not *fast* — the measured median is ~2s and the tail reaches 8s, which is fine for
fire-and-forget work and would be poor for a button the user watches. Matches the convention in
`ContextBreakdown.svelte:10` ("opening is the refresh"). Do not add one.

### Non-goals

- **Claude's ingestion source is untouched.** Its stream payload names every window it reports, so
  no new call is added for it. What M3 changes is how readings are *combined*, not where they come
  from.
- **The model reserve is not rendered**, and `GPT-Reserve` is not added to the picker. Decided, not
  deferred.
- **`rateLimitUpsell` is not rendered.** It is entirely about the reserve; it is `dismissible` with
  `ctas` including "Add Credits", making it a vendor marketing surface; and its `description`
  carries a `{time}` placeholder, so rendering it means maintaining a template renderer for copy
  that can change without a version bump and would have no drift test. Its shape is recorded in M5.
- **`rateLimitResetCredits` is not rendered.** The account currently holds an unconsumed free reset;
  spending one is a real action with real consequences and deserves its own decision. Shape recorded
  in M5.
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

**The behavior behind M2's rendering rule — now probed, and the assumption held.** The question was
narrow, and it was **not** "record the healthy shape" generally:

> Does a **refreshed** bucket — new `resetsAt` in the future, `usedPercent: 0` — still carry a
> leftover `rateLimitReachedType`?

A window-scoped exhaustion cannot go stale *within* its window (capped stays capped until reset),
and once the reset passes M2's reset-passed rule drops the bucket along with its flag. The refreshed
bucket was the only residual.

**Answered on 2026-09-19, when the account's weekly window rolled over: the flag clears.** The
recovered `codex` bucket reports `usedPercent: 0` against a new `resetsAt` with
`rateLimitReachedType: null`, and `ordinaryUsageAllowed` flips to `true`. The flag tracks the live
window rather than latching on the account, so a recovered quota stops rendering as blocked on its
own. The capture is pinned as `account-rate-limits-healthy.jsonl` and asserted by
`a_recovered_account_clears_the_exhaustion_flag_rather_than_leaving_it_set` — it took a real reset to
obtain and cannot be reproduced on demand, unlike the exhausted shape.

**So M2's `usedPercent > 0` conjunct is redundant rather than load-bearing.** Keep it: it costs
nothing, and it still guards the one shape we have seen only once (a windowless bucket). This
paragraph is what lets a future reader delete it deliberately instead of inheriting it as folklore.

Other unprobed items — logged-out and offline responses, bucket sets on plans other than `prolite` —
are handled as generic failures and recorded in M5 rather than designed for.

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

**Ingestion cut happens here, not in the deletion milestone.** This is load-bearing sequencing. `recordAccountUsage`
(`src/lib/state/index.svelte.ts:982`) routes every Codex `rate_limit_event` into `observeUsage`
stamped with **arrival time**, so it wins newest-wins over the account read every time;
`recordRestoredUsage` (`:429`) does the same on project open; `codex/mod.rs:821` still emits after
each `TurnEnd` until M4. If the frontend cut waits for M4, the Codex section clean-hides after every
turn and every project open for the whole duration of M2 — worse than the bug being fixed. **Both
frontend entry points stop routing Codex to the store as part of this milestone.**

**Call sites.** Startup (gated per M1), after each Codex turn ends, and on panel visibility.
Visibility matters for a specific reason: a blocked user's bucket renders fine until its reset
*passes*, and the section then empties at exactly the moment they are watching it — and they are not
ending turns, so nothing else would refresh. Not window focus, not a timer. This is the same
"opening is the refresh" rule cited above.

**Wire `HarnessKind::supports_account_usage_read` to a real caller here.** M1 landed the predicate
with none, which is normal for a milestone that ships before its consumer but is exactly how a
predicate becomes permanently decorative. Two callers, and the first must not be a formality:

- **Backend — the startup gate.** It decides whether Codex is in use at all, so it should ask the
  predicate of each configured agent's harness rather than matching on `HarnessKind::Codex`. Calling
  `HarnessKind::Codex.supports_account_usage_read()` at a site that already knows it is Codex would
  satisfy the letter of this and nothing else; do not do that.
- **Frontend — the mirror.** Add `supportsAccountUsageRead` to `src/lib/harnessCapabilities.ts`
  beside `supportsManualCompaction` / `supportsContextReport`, and gate which harness sections a
  self-refreshing account read may serve. The mirror is the established two-sided pattern; a
  backend-only predicate leaves the UI deciding by harness name.

**The command stays Codex-named and takes no `harness` parameter.** Reviewed and rejected: it speaks
a protocol only Codex has, so a harness argument would build a dispatcher with one arm and a
signature that promises a generality no second harness can supply. The capability gate belongs at
the caller, which is what the predicate is for.

Fire-and-forget; never on a turn's critical path.

**Coalescing — drain semantics, not dedupe.** Mirror `harnessUsage.svelte.ts::persist` (`:222`)
*including* its follow-up: a request arriving while one is in flight sets a flag and triggers
exactly one further call. This is not a technicality — a read that started before a turn ended
returns a number predating that turn's consumption, so answering the later request from the in-flight
call would systematically understate usage.

**Size it for a read that is usually already running.** The measured distribution above (median
2.07s, p90 5.63s) against a trigger that fires at every turn end means the in-flight case is the
**normal** path, not the burst path. Do not reason about this as "several agents finishing at
once" — assume a read is in flight most times one is requested, and that the follow-up flag is set
on the majority of triggers rather than rarely.

**Superseding an in-flight read by discarding it is not available.** The mirror *awaits* the in-flight
call, and Tauri propagates no cancellation, so nothing on either side drops the future — which is why
`ACCOUNT_USAGE_TIMEOUT` is load-bearing rather than defensive: an unbounded read against a wedged
server would hold the single in-flight slot for the life of the session and permanently disable the
Codex meter. If a later revision wants supersede-by-discard, `read_account_usage`'s cancellation
handling is what makes that safe, and adopting it is a deliberate change rather than a free option.

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

The `usedPercent > 0` conjunct was written as the guard for the refreshed-bucket question, and it is
deliberately *not* a threshold: a quota that is 0% spent and simultaneously exhausted is a
contradiction, so when the flag and the measurement disagree we trust the measurement and claim
nothing. No arbitrary percentage is invented. **The probe has since come back clean** — a recovered
bucket clears its flag (see M1 above), so the conjunct now guards a shape Codex does not currently
produce. It stays because it is free and still covers the windowless bucket; it is no longer the
thing standing between a recovered account and a false "blocked".

Two things to be honest about in the code comment, not only here. This **infers recovery from a
percentage**, which the schema's own wording tells clients not to do ("must not infer recovery from
percentages or reset times") — accepted knowingly, as the narrowest available guard against a
failure mode we cannot yet rule out. And it errs toward **understating** a quota, which is the
direction this codebase takes throughout: a meter that fails to shout is recoverable, one that
falsely claims you are blocked is not.

**State its coverage honestly: it does not make the probe optional.** The guard only catches a read
taken *before any turn runs in the new window*. One turn in, `usedPercent` is a few percent and a
stale flag sails through, rendering a nearly-empty bar in the warning tone. Since a turn ending is
itself what triggers a read, that is the common case rather than the rare one. What the guard
reliably protects is the read a *waiting* user is most likely to be looking at.

**Staleness copy — do nothing here.** `HarnessUsage.svelte:214` currently reads "Measured
{relativeTime} — **send a message to refresh**", which becomes false for Codex once the app refreshes
itself. **Leave it untouched anyway.** M3 removes the imperative for *both* harnesses and moves the
timestamp per-window, so a per-harness variant built here is exactly the throwaway intermediate shape
this instruction exists to prevent. These milestones are commits in one PR, not separate review
units, so there is no user-visible window in which the Codex line is wrong.

**Do not touch `asReading`** beyond removing `limit_reached`. M3 extends its whitelist for
per-window provenance; splitting that across two milestones is how the gate-lost-on-restart class of
bug happens.

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
- `supports_account_usage_read` has a non-tautological backend caller and a
  `harnessCapabilities.ts` mirror, per the call-sites section above.

---

## M3 — Keep Claude's windows until they reset, and date each one

### Goal & Outcome

Stop a reading that does not mention a window from deleting that window.

- A Claude window stays on screen until **its own reset passes**, rather than until some later turn
  happens not to mention it.
- A user who hits the model-gated weekly cap on Fable and then works on Opus still sees the Fable
  cap, because it is still in force.
- Each window's tooltip states when **that window** was last measured, since after merging they no
  longer share one instant.
- No window is ever shown with a number from a reading that did not contain it.

### Implementation Outline

**The defect.** Claude's `unifiedWindows` is *partial*: it omits `seven_day_overage_included` unless
the turn ran on the gated model. The store replaces whole readings, so an Opus turn silently deletes
a Fable cap that is still blocking work. Verified on this machine — two installs, same account, same
moment:

```yaml
# last turn Opus                      # last turn Fable
model: claude-opus-5                  model: claude-fable-5-1
unifiedWindows:                       unifiedWindows:
  five_hour: 0.07                       five_hour: 0.28
  seven_day: 0.73                       seven_day: 0.70
  # Fable cap absent                    seven_day_overage_included: 1
```

This is **not** an agent- or model-based filter in our code — `claudeRateLimitView` walks a fixed key
allowlist that already includes the gated window and renders whatever the payload holds. The loss is
purely the whole-reading replacement.

**Why the fix is the opposite of the Codex rule, and why that is not a contradiction.** The governing
question is whether a new reading is *complete*, and whether its parts are *identifiable*:

| Reading | Complete? | Identity carried *in the reading*? | Correct policy |
| --- | --- | --- | --- |
| Codex via `account/rateLimits/read` | yes | yes — `limit_id` | replace; nothing is missing |
| Claude `rate_limit_event` | **no** | yes — the key *is* the identity | **merge per key** |
| Codex via rollout | no | **no** — inferable only from a neighbouring record, by an unestablished rule | neither works; replace the source (M1/M2) |

**The second column is deliberately "carried in the reading", not "knowable".** An earlier draft said
the rollout's parts were simply unidentifiable, and that is too strong: `turn_context.model`
co-varies with the bucket at the one observed transition and `gpt-reserve` matches the API's
`limitName` exactly. A future reader who checks the table against a rollout will find that model
slug and conclude the table is wrong — which is the "fix one harness to match the other" outcome it
exists to prevent. The honest claim is that identity would have to be *reconstructed* from an
adjacent record by a rule that is not established (`gpt-5.6-luna` appears against both a reserve
bucket and a windowless `premium` record).

Note also that the rollout fails the **first** column regardless: one bucket of N can never be a
complete reading, so even perfect naming would not rescue it. That argument is unaffected by how the
identity question resolves, and it is the one to lean on.

One premise worth flagging rather than asserting: "Codex via API · complete: yes" rests on a single
capture on one plan, and M1 itself records that bucket sets on other plans are unknown. The schema's
wording ("multi-bucket view keyed by metered `limit_id`") makes it a contract claim rather than an
observation, which is fair — but it is the only cell with **no detection if its premise is wrong**,
since `replace` silently drops a bucket that stops appearing.

State this table in the code, not only in this plan — it is the thing that stops a future reader
"fixing" one harness to match the other.

**Store provenance, never a verdict.** Per window key, store the vendor's own window object
**verbatim** plus the context of the reading that delivered it — `observed_at`, `model`, and the
account-level trio that window's flags are derived from (`status`, `rateLimitType`,
`surpassedThreshold`). Account-level fields for the *card* (`isUsingOverage`, `overageResetsAt`,
fallback) come from the newest reading only and are stored separately.

`claudeRateLimitView` then derives each window's flags at render from that window's own delivering
context, exactly as it does today. **Nothing in the store is a conclusion.**

This is not a stylistic preference. Today every displayed number is re-derived from verbatim payloads
at render, so an interpretation bug is corrected *retroactively* for data already on disk — and this
plan exists because of two interpretation bugs. Computing flags at ingest would freeze a bad
judgement into `usage.yaml` until each window resets. Storing slices of vendor data plus who
delivered them keeps that property, keeps convention 2 literal rather than caveated, and leaves the
store's only new knowledge as "which keys exist", which merging inherently requires.

It also removes a seam: with flags derived at render for both harnesses, there is one mechanism
behind the visual rather than Codex deriving and Claude storing.

**Why retaining a window's flags is safe here, and was not for Codex.** A retained window keeps the
threshold and refusal judgement that was true when it was observed, because those are judgements
about *that window* and remain true until it resets. Structurally this is the carry-forward M2
deletes for Codex, and the difference is not merely that the key is stable:

- Claude's `status` is **account-level**, so an `allowed` reading delivered by an Opus turn is
  evidence about the windows that turn touched — not about the Fable cap. Retention is the reading
  that *doesn't* over-generalise.
- Every Claude window carries a numeric `resetsAt` or `claudeRateLimitView` drops it, so unlike
  Codex there is no reset-less window a flag can sit on indefinitely.
- The two-event refused-turn ordering recorded at `harness-behavior.md:695` survives merging intact
  and is *improved* by it: the opening `allowed` event omits the gated key entirely, so it updates
  the other windows and leaves the verdict alone. The residual noted there — "a turn killed between
  the two events leaves the cheerful reading held" — also shrinks, because a killed turn can no
  longer delete the gated window. **M3 should claim that improvement rather than leaving it
  implicit.**

**Window instance changes replace; they do not merge.** A reading whose `resetsAt` for a known key
differs from the held one describes a **new instance** of that window and replaces it outright,
dropping the previous instance's flags — bypassing `observed_at` ranking entirely. This matters in
one case and it is the case ranking gets wrong: an undated or lower-ranked reading carrying a freshly
cycled window.

Storage stays keyed by **window key**. `resetsAt` is a same-key check, *not* part of the identity —
treating `(key, resetsAt)` as the identity would imply two stored entries for one window and require
something to arbitrate between them, which is worse than today.

This also protects the "no expiry needed" argument: utilization only climbs *within* a window, so a
retained value understates. Across a reissued window that invariant does not hold, which is exactly
why a changed reset must replace.

**Staleness otherwise fails safe.** The existing reset-passed rule drops a window once it cycles,
which retires its flags with it. No age threshold is needed.

**One assumption to state rather than bury: `isUsingOverage` is treated as account-wide.** Taking it
from the newest reading is only correct if it describes the account rather than the window that
triggered it. If it were window-scoped, a Fable turn could put the account in overage with the gated
window at 100%, an Opus turn report `isUsingOverage: false`, and the ⚡ escalation vanish while the
retained window stays — the same "later reading deletes still-true state" defect this milestone
exists to fix, relocated to the account fields and invisible because the window keeps rendering.
**Unprobeable on this account** (both stores read `overageDisabledReason: org_level_disabled`), so it
goes in M5's gap register with what would close it. M3 established that `unifiedWindows` is partial;
it has not established that its siblings in the same payload are not.

**Per-window provenance, and the label race this creates.** The gated window has no label of its
own; it is named from the model of the turn that delivered it, so that model must ride with the
window rather than with the reading.

**This turns an accepted race into a durable defect, and fixing it is not optional.**
`index.svelte.ts:1001-1005` documents the trade today: two agents interleaving between one turn's
rate-limit event and the other's `init` can label a window with the wrong model, and the comment
prices that as *"strictly better than dropping the label."* **That pricing assumed whole
replacement**, where the mislabel survives one reading — seconds. Under merge the window outlives the
reading and `nameUsageModel` never relabels a filled slot, so:

> Agent A (Fable) emits its rate-limit event before its `init` lands → the gated window stores
> unlabelled → Agent B (Opus) `session_meta` arrives → the Fable cap renders "Weekly · Opus" for up
> to seven days.

The correction path is closed by the same condition that creates the state: only another gated-model
turn can fix it, and the user is capped on that model.

**Fix: scope the fill to its producer.** Record on the window the agent whose event contributed it,
and have `nameUsageModel` fill only blanks contributed by *that* agent's most recent event. Both call
sites already hold `agentId` (`recordAccountUsage`, `:1006`). **Re-price the comment at `:1001`** — it
currently justifies a risk at a duration that no longer applies. No information is lost: the label
comes from the delivering agent or not at all, and a window whose deliverer never reported a model
renders unlabelled rather than wrong, which is the rule this surface follows throughout.

**The fallback path changes meaning and the plan must say so.** `claudeRateLimitView` currently gates
its bare `resetsAt`/`rateLimitType` fallback line on the **newest reading's** `unifiedWindows` being
empty. After merging, windows live in the store, so a reading with an empty map would render the
fallback line while retained windows are still live. The rule becomes: **the fallback applies only
when the merged window set is empty**, not when the newest reading's map is. This is a new state that
merging creates, and it lands on the same "an empty container counts as absent" comment that already
reasons carefully about the reading-level case.

**Persistence nests inside `payload`.** `asReading` (`harnessUsage.svelte.ts:184-200`) is a hard
whitelist — `payload` is copied wholesale and anything beside it is dropped. Per-window provenance
must live inside `payload` or it is silently stripped on every load. This is the same trap as the
previous round's gate-lost-on-restart finding; M3 owns extending the whitelist for its own fields
(M2 must not touch `asReading`). M2 deletes `limit_reached` from it.

**Tooltip.** Replace the single harness-level "Measured … ago" line with a per-window line, because
merged windows genuinely have different measurement times and one line for all of them would be
false.

**Drop the "send a message to refresh" imperative for both harnesses.** It is not merely stale for
Codex — after merging it becomes *wrong for Claude, in exactly the situation this milestone exists
for*: sending a message refreshes only the windows that turn's model touches, and the gated window is
refreshed by a gated-model turn and by nothing else. A user staring at a stale Fable cap would be
told to send a message, do it, and watch that instant not move. The footer instead states the real
rule — that each window updates when a turn runs against it — which explains why the instants differ
and is true for Codex too, whose windows all update together because one call refreshes them.

Per-window instants are used for both harnesses. Codex's will read identically since they arrive from
one call; that is truthful and is preferred over a layout that changes shape depending on whether the
values happen to agree.

**Persistence.** Stored Claude entries in the old shape carry no per-window instants. Treat a missing
per-window instant as unknown and render that window without an age line rather than backfilling it
with the reading-level timestamp, which would date a window by when a *different* window was
measured. The next turn repairs it.

### Definition of Done

- A reading omitting a window does not remove it; the window survives with its previous value, its
  own instant, and its own flags.
- A reading *containing* the window updates it, including clearing a threshold flag the new reading
  no longer reports.
- A window whose reset passes drops, taking its flags with it, even if no newer reading has arrived.
- The gated window keeps its model label after a later reading from a different model.
- `nameUsageModel` fills the label on the correct window when `init` arrives after the rate-limit
  event, and a later turn on a different model does not relabel it.
- **Two agents interleaving across the event/`init` boundary do not cross-label**: a second agent's
  `session_meta` never names a window contributed by a different agent.
- A reading whose `resetsAt` for a known key differs replaces that window and drops its flags, even
  when the reading is undated or would lose on `observed_at`.
- Account-level card state — overage escalation and the fallback line — reflects the newest reading
  only and is not merged.
- **The fallback line renders only when the merged window set is empty**, not when the newest
  reading's map is; retained windows suppress it.
- Tooltip: per-window measured lines, no refresh imperative for either harness; a window with no
  known instant shows none.
- **Persistence round-trips.** A *new*-shape entry survives `set_harness_usage` /
  `get_harness_usage` with per-window instants, models, and delivering context intact — a case
  distinguishable from the old-shape one below, which the previous DoD wording was not: "loads
  without inventing instants" passes identically whether persistence works or strips every entry on
  every restart.
- An old-shape persisted entry loads and renders without inventing instants. It still retires
  correctly, because the reset rule is per-window and reads the payload's own reset — only the age
  *line* is unavailable.
- The completeness/identity table is recorded in the code.

---

## M4 — Delete the Codex rollout rate-limit path

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

## M5 — Correct and close the harness record

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
- **G8**, which describes the rollout-derived Codex rendering M4 deletes.
- **§1.4** quick-reference.
- A version-log entry for the discovery, including the probe table (unpinned refused, `gpt-reserve`
  succeeded, `gpt-5.6-luna` refused) and the conclusion that `normalModelSlug` is a display alias
  rather than a dispatchable slug.
- **The Claude partial-payload finding (M3) — record the *consequence*, not the fact.** The fact is
  already documented: §3.12 (`:549-550`) states that `seven_day_overage_included` "appears only on
  turns run on one of them", and the version log at `:699` calls it model-gated while `:695` records
  a captured payload with "no `seven_day_overage_included` key at all". **Do not "correct" those
  entries — they are right.** This is the second entry in this plan that was called wrong when it was
  accurate-but-incomplete (the §5 Codex entry was the first); treat that as a pattern to check for
  before editing anything in M5.

  What was never written down is that whole-reading replacement therefore *deletes a window still in
  force*. Add that to §3.12 as a line saying a reading is a partial view of the account and must not
  be treated as a replacement, and add the omission caveat to **§1.4**, which does lack it — it
  describes `unifiedWindows` as a map of key → `{utilization, resetsAt}` and documents the warning
  event's map as a superset, with no hint the base map can omit a window.

  **Frame it as G7 one level down, which is both accurate and stronger than a new discovery.** §1.4
  (`:579`) already records that "a record carrying no `rate_limit_info` emits no event at all, so it
  cannot overwrite that snapshot with nothing" — the *absence is not data* rule. It was applied at
  the reading level and stopped there; M3 applies it at the window. That is why this bug survived in
  a well-documented harness.

- **Two entries whose rationale goes stale under merging**, both needing a clause rather than a
  rewrite: `:581`, whose reason for skipping an empty `rate_limit_info` ("would erase every window
  from memory *and* from disk") is half-false once an empty record is a no-op rather than a wipe —
  the rule stands, the memory half of the reason does not; and `:695`, whose "that ordering is what
  makes newest-wins safe on this payload" becomes a statement about *per-key* newest-wins.
  `:695` should also be cited in M3 as pre-existing corroboration for the partial payload: it
  captures the Fable wall as `seven_day_overage_included: 1` alongside `five_hour: 0.28` /
  `seven_day: 0.70`, which is the "last turn Fable" column independently.

**Record in the gap register, because the captures are perishable and re-capturing needs another
wall:** the `rateLimitUpsell` shape (including `banner_type`, the `{time}` placeholder, `ctas`, and
`reset_at` pointing at the blocking quota), the `rateLimitResetCredits` shape, and M1's unprobed
items with what would close each.

**`README.md` "Harness support and limitations"** gains a short user-facing entry: when the Codex
weekly allowance is spent, Switchboard shows it as spent and every model in the picker is refused;
running the task in the Codex CLI directly still works, because it offers a reserve model Switchboard
does not expose. Symptom first, one or two plain lines. **Verified**, not inferred — the interactive
CLI reports the exhaustion and offers the switch, and a task was completed that way. Keep it accurate
on that point: the probe table's "unpinned is refused" result is about `codex exec` and does not
describe the interactive CLI.

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
4. ~~**The refreshed-bucket question ships unprobed.**~~ **Closed 2026-09-19.** The account's weekly
   window rolled over and the probe ran: a recovered bucket clears `rateLimitReachedType` and flips
   `ordinaryUsageAllowed` to `true`, which is the assumption M2's rule was written against. No rule
   change; the observation is recorded in M1 and pinned by a fixture test.

---

## Pre-PR verification

**Both gates are discharged as of 2026-09-19.** Each needed a real account state that could not be
conjured on demand, which is why they gated the PR rather than the work; the account's weekly window
reset and both ran together.

1. **The refreshed-bucket probe — run, assumption held.** A recovered `codex` bucket reports
   `usedPercent: 0` against a new `resetsAt` with `rateLimitReachedType: null`, and
   `ordinaryUsageAllowed` reads `true`. That is the "flag clears on reset" branch: the
   `usedPercent > 0` conjunct is redundant but harmless and stays. The capture is checked in as
   `account-rate-limits-healthy.jsonl` rather than described only in prose, because the exhausted
   shape can be reproduced by spending quota and this one cannot be reproduced at all.

   The same read also settles the open question about `ordinaryUsageAllowed`: it is non-null on a
   *healthy* account, not only on a capped one, so the live test's `is_some()` assertion is now
   confirmed in both states rather than in one.

2. **`make test-live-codex` — passed 18/18** on 0.155.1: the account usage read plus the
   pre-existing adapter coverage (dispatch, resume, `apply_patch`, hydration, tool use, transcript
   load, cancel, attachments, auth check).

Carry both outcomes into M5's gap-register entries; they are recorded here so the answer is not lost
if that milestone is reordered.
