# In-flight turn hydration: stable identity + convergent merge

**Status:** Proposed
**Date:** 2026-06-07
**Branch:** `fix/in-flight-turn-hydration-dedup`

## Problem

A single Claude agent turn can render **twice** in the unified transcript: once
complete (intro text + tool calls + final answer) and once as a frozen
duplicate with the last tool stuck "spinning." Reproduced from a real session
(`switchboard-images` project, agent `019ea2fd-1679-…`); the harness session
file itself is clean — the duplication is entirely in how Switchboard
reconstructs and merges turns.

### Root cause (verified)

A Switchboard *turn* spans 1..N Anthropic assistant messages. In the reproducing
trace the turn spans two: `msg_017…` (`stop_reason: tool_use`, carrying the
intro text + a `Read` + two `Agent` tool calls) and `msg_0157…`
(`stop_reason: end_turn`, the final answer). The whole live↔disk dedup rests on
one identity string — `hydration_key` — matching between the live-streamed turn
and the disk-parsed turn. Today that key is **the turn's final assistant
`message.id`**, on both paths (`crates/harness/src/parser.rs:337` live;
`crates/harness/src/claude_code/session_file.rs:485` disk; aliased onto the wire
in `crates/harness/src/events.rs:485`).

That key is only stable for a *finished* turn. `last_message_id` is overwritten
per assistant record, so a parse that catches the turn **mid-flight** anchors on
the intermediate `msg_017…`, while the completed turn anchors on `msg_0157…`.
The two never dedup, so both render. Proven by running the real parser
(`load_claude_transcript`) on the actual file truncated mid-flight vs. complete:

| parse | `hydration_key` | tools |
| --- | --- | --- |
| mid-flight (through the two `Agent` `tool_use`, before their results) | `msg_017…` | last two `Agent` tools unresolved → spinning |
| complete | `msg_0157…` | all resolved |

### Dominant trigger: switch-back refresh during an in-flight turn

The most frequent real-world repro is **switching between projects while a turn
runs in the background**, which makes the duplication *accumulate*: more blocks
per switch. Verified mechanism (`src/lib/state/workspace.svelte.ts`):

- On switch-back, `maybeRefreshProject` re-hydrates a project when any
  **refresh-capable** agent's session file changed since last view
  (`:797–798`). `refresh_capable = harness.supports_refresh()` is **per-harness**
  and **true for Claude** (`crates/app/src/commands.rs:2708`, test `:5950`).
- An in-flight turn's file grows continuously, so its `stat` fingerprint changes
  on every switch-back → every switch-back re-hydrates that agent.
- Each re-hydration re-merges the in-flight tail turn anchored on the **latest
  assistant message id at that instant**. A turn moves through several assistant
  messages (each tool round = a new `message.id`), so successive switch-backs
  catch *different* anchors → different `hydration_key` → first-write-wins
  **adds each as a new block**. Duplicate blocks accumulate, one per distinct
  anchor caught.

The refresh path's "dup-safe via the stable key" guarantee
(`hydrateProject` docstring) holds only for **completed** turns; it was never
designed for an in-flight turn whose key isn't yet stable. Same root cause as
below, with switch-back as a high-frequency repeat trigger.

Two distinct defects feed the visible duplicate; both must be fixed:

- **Hole A — identity is not parse-invariant for an in-flight turn.** The
  anchor (`last_message_id`) is a moving target until the turn truly ends.
- **Hole B — the hydrate merge is first-write-wins.**
  `crates/harness`/frontend `reducers.ts` hydrate arm keeps whatever is already
  in the slice and *drops* a colliding incoming turn
  (`src/lib/state/reducers.ts:273–277`). Even once identities match (Hole A
  fixed), a frozen partial that landed first would keep winning over the later
  complete parse.

Underneath both is one missing notion: **disk state alone can't distinguish a
turn that is still being written from one that ended.** The parser's own comment
says so (`session_file.rs:491–496`). We close the gap with a signal the parser
currently ignores — the **last assistant message's `stop_reason`** — plus a
start-stable identity. Each assistant record carries a definite `stop_reason`
even for its intermediate content blocks (verified in the trace), so the last
assistant record's `stop_reason` is a reliable "is more coming?" signal:
`tool_use` ⇒ the model owes a continuation (not finished); `end_turn` /
`max_tokens` / `stop_sequence` ⇒ terminal.

### Chosen approach

1. **Decouple turn *identity* from the cost-join key.** Keep
   `stable_message_id` = the **final** assistant `message.id` (it is the
   cost/overage sidecar join key — cost lives on the final record; do not
   touch it). Make `hydration_key` = the turn's **first** assistant
   `message.id` — present the moment the turn produces any output, invariant
   for the rest of the turn, and therefore identical between a mid-flight and a
   complete parse. This is the keystone; it stops the duplicate twin from being
   *created*. It also aligns Claude with Codex, which already anchors
   `hydration_key` on a turn-start-stable id (`turn_context.turn_id`,
   `crates/harness/src/codex/session_file.rs:705`) — so this is bringing the
   outlier into line with the existing cross-harness contract, not inventing a
   pattern.

2. **Mark a non-terminal tail turn from disk as `Streaming`, not `Complete`,**
   using the last assistant `stop_reason`. Only the EOF tail turn can be
   non-terminal; every earlier turn is closed by a following user prompt and
   stays `Complete`.

3. **Replace first-write-wins with completeness-ranked supersession** in the
   hydrate merge: a terminal incoming turn supersedes a non-terminal resident of
   the same identity (merging the resident's live-only fields the disk parse
   lacks); otherwise keep the resident (preserving the existing "live turn wins
   over a disk re-read" intent).

### Why *not* more

We are **not** building the dispatcher↔frontend resync / turn-liveness-ownership
protocol (call it "fix c" in the investigation). It would let a reload-stranded
partial heal *live* and would make the merge precedence fully principled, but it
is a real cross-process protocol and is not required to fix the reported bug.
Its absence is recorded as a known limitation (see end of plan). This is a
deliberate scope boundary, per "match complexity to the problem."

### Docs to read before implementing

- `docs/system-design.md` §3 (split conversation source-of-truth: harness files
  own completed-turn content; the journal owns non-completed outcomes) and §7
  (sends vs. turns).
- `docs/research/harness-behavior.md` — current Claude adapter behavior and the
  gap register; add an entry there (see DoD).
- Anthropic stop-reason reference — the authoritative, current list of
  `stop_reason` values and their meanings. **Read this before writing the M2
  classifier** and pin the terminal/continuation sets against it (it lists
  `pause_turn`, a continuation state that a naive "not `tool_use` ⇒ terminal"
  rule would misclassify):
  https://platform.claude.com/docs/en/build-with-claude/handling-stop-reasons
- The two existing anchor tests, which encode the current (to-be-refined)
  contract and the exact bug shape:
  `crates/harness/src/parser.rs::tool_use_turn_anchors_stable_id_on_final_assistant_message`
  and
  `crates/harness/src/claude_code/session_file.rs::hydrated_tool_use_turn_anchors_stable_id_on_final_assistant`.

---

## Milestone 1 — Stable turn identity, decoupled from the cost-join key

### Goal & Outcome

Make a Claude turn's `hydration_key` its **first** assistant message id on both
the disk and live paths, while `stable_message_id` continues to be the final
assistant message id for the cost sidecar. After this milestone the identity is
parse-invariant: the same turn yields the same `hydration_key` whether parsed
mid-flight or complete, and whether produced live or from disk.

Outcomes:
- Parsing a turn from disk twice — once truncated mid-tool, once complete —
  yields the **same** `hydration_key`.
- A live-completed turn and a later disk hydration of that same turn carry the
  **same** `hydration_key`.
- `stable_message_id` (and therefore the cost/overage sidecar join) is
  unchanged: still the final assistant message id.
- Non-Claude harnesses are behaviorally unchanged.

### Implementation Outline

This is a contract change spanning disk + live; both must move together, because
the hydrate merge compares a live turn's key against a disk turn's key and they
must agree.

- **Disk parser (`claude_code/session_file.rs`).** The turn builder already
  keeps the *last* assistant message id; additionally capture the *first*
  assistant message id of the turn (keep-first: set only when unset, reset
  per turn). On close, set `hydration_key` = first id, `stable_message_id` =
  last id. The first assistant record is non-subagent by construction —
  subagent records are never inlined in the main session file (documented at
  `session_file.rs:1385–1396`), matching the live parser's non-subagent
  selection — so no sidechain filter is needed, but preserve that property if
  the surrounding code changes.

- **Live parser (`parser.rs`).** Mirror the disk change: alongside
  `last_assistant_message_id`, track the first non-subagent assistant message
  id (subagent envelopes are already skipped here via `parent_tool_use_id`).
  Emit it on `AdapterEvent::TurnEnd` as a new, dedicated field (the turn's
  identity) — distinct from `stable_message_id`, which stays the final id for
  cost. In M1 emit identity at `TurnEnd`; M4 adds an *earlier* emission (at the
  first assistant message) so a live turn carries its identity before it ends.
  M1's `TurnEnd` emission then doubles as M4's graceful-degradation backstop.

- **Event contract (`events.rs`).** `AdapterEvent::TurnEnd` gains the identity
  field (`Option<String>`). The `AdapterEvent → NormalizedEvent` conversion
  routes **the identity field** into `NormalizedEvent::TurnEnd.hydration_key`
  (today it routes `stable_message_id`; that alias is the bug). `stable_message_id`
  stays internal/dropped at the wire boundary as before. Document on the new
  field that identity = first assistant message id (start-stable, dedup key)
  and `stable_message_id` = final (cost-join key), and *why* they differ — this
  rationale must live in the field doc-comment, not just here.

- **Other `AdapterEvent::TurnEnd` construction sites** (codex, gemini,
  antigravity, mock adapters) pass `None` for the new identity field — no
  behavior change. Matchers that destructure `TurnEnd` with `..` are
  unaffected; only construction sites need the new field.

- **Frontend** carries `hydration_key` through unchanged
  (`reducers.ts` turn_end arm and `loadedTurnToTurn`); no frontend logic change
  in this milestone — only the *value* flowing through it changes. Confirm the
  wire type (`LoadedTurn.hydration_key`, `NormalizedEvent`/`Turn` types) needs
  no shape change.

Load-bearing decisions (cannot be recovered from code): `hydration_key` must be
the **first** assistant id and `stable_message_id` the **final** id — they are
now intentionally different values; the previous "they coincide for Claude"
comments are obsolete and must be corrected, not preserved. Three stale sites
all assert the *final* id is the live↔disk dedup key and must be updated to the
*first* id (the final id remains only the cost-join key):
- `claude_code/session_file.rs:482–484` (the parser's `hydration_key` comment).
- `events.rs:462` (the live `TurnEnd` conversion comment).
- **`crates/core/src/harness.rs:70–82`** — the `supports_refresh` doc, which is
  the canonical rationale a future harness author reads to decide refresh
  eligibility ("Claude is confirmed (final assistant `message.id` round-trips
  live↔disk)"). After M1 the round-tripping id is the **first** assistant
  `message.id`; leaving this stale could lead someone to qualify a new harness
  against the wrong anchor. This one is load-bearing, not cosmetic.

### Definition of Done

- **Disk unit tests:** the same turn parsed mid-flight (truncated mid-tool) and
  complete yields identical `hydration_key`; `stable_message_id` differs
  (intermediate vs final) and still equals the final id on the complete parse.
  Update `hydrated_tool_use_turn_anchors_stable_id_on_final_assistant`: it must
  still assert `stable_message_id == msg_test03` (final) **and** newly assert
  `hydration_key == msg_test02` (first). The mid-flight-vs-full parse from the
  investigation is the canonical new test.
- **Live unit tests:** update
  `tool_use_turn_anchors_stable_id_on_final_assistant_message` so
  `stable_message_id` stays the final id while the emitted identity/`hydration_key`
  is the first id.
- **Cross-path test:** a fixture where live `TurnEnd.hydration_key` equals the
  disk `hydration_key` for the same turn (first id on both).
- **Non-Claude:** existing codex/gemini/antigravity/mock tests still pass with
  the new `None` identity field.
- **Docs corrected:** the three stale "final id is the dedup key" comments above
  (`session_file.rs`, `events.rs`, and the `supports_refresh` doc in
  `crates/core/src/harness.rs`) now reference the first assistant `message.id`.
- `make check` green.

---

## Milestone 2 — Non-terminal tail turns from disk via `stop_reason`

### Goal & Outcome

Teach the disk parser to recognize when the **last** turn in a session file did
not actually finish, and mark it `Streaming` (in-progress) instead of falsely
`Complete`. The signal is the last assistant record's `stop_reason`.

Outcomes:
- A session file whose tail turn ends on `stop_reason: tool_use` (model owes a
  continuation) hydrates that turn as `Streaming`, with its unresolved tools
  rendering as in-progress.
- A session file whose tail turn ends on a terminal `stop_reason`
  (`end_turn` / `max_tokens` / `stop_sequence`) hydrates it as `Complete`.
- Every non-tail turn (closed by a following user prompt) remains `Complete`,
  exactly as today — a new prompt is definitive proof the prior turn ended.

### Implementation Outline

- **Track the last assistant `stop_reason`** in the turn builder (keep-last,
  same discipline as the message id). The parser does not read `stop_reason`
  today; add it.
- **Status decision is split by close-reason:**
  - Closed by a following user *string* prompt (the existing turn boundary in
    `handle_user`) → `Complete` (unchanged).
  - Closed by `finalize` at EOF (no following prompt) → `Complete` iff the last
    assistant `stop_reason` is terminal; otherwise `Streaming`. As a
    belt-and-suspenders consistency check, an unmatched/open `tool_use` at EOF
    also implies non-terminal — but `stop_reason` is the primary signal and
    subsumes the gap that "open tools" alone misses (the window where tools are
    balanced but the next assistant message hasn't been written).
- This means only the EOF tail turn can ever be `Streaming` from disk, which is
  correct: it is the only turn that can be mid-flight.
- Update the `finalize` doc-comment (`session_file.rs:491–496`): the old claim
  "truncated mid-turn and completed-but-no-next-prompt-yet are indistinguishable
  from disk" is now resolved by `stop_reason`; replace it with the new rule and
  *why* (the rationale must survive into the comment).

Edge cases / constraints we identified:
- **Classify with explicit positive allowlists, never a negative check.** A rule
  like "anything that isn't `tool_use` is terminal" makes the *unsafe*
  classification (terminal) the default for every present-and-future value — the
  wrong direction for a forward-compatible parser, and it misclassifies
  `pause_turn` (a real continuation state for long-running server-side tools) as
  `Complete`, recreating the exact bug this plan fixes. Instead:
  - terminal (→ `Complete` at EOF) ⇒ `{end_turn, max_tokens, stop_sequence}`;
  - continuation (→ `Streaming` at EOF) ⇒ `{tool_use, pause_turn}`;
  - unknown / null / any unlisted value ⇒ `Streaming` (conservative — an absent
    or unrecognized terminal marker is not proof of completion).

  Pin the exact membership against the Anthropic doc cited above at
  implementation time (e.g. decide whether `refusal` should join the terminal
  set; the conservative default leaves it `Streaming`, which only over-renders a
  spinner — it never falsely marks an unfinished turn complete).
- This milestone does **not** attempt to distinguish "running right now" from
  "crashed/cancelled on a tool round" — both look like a trailing `tool_use` on
  disk. Both map to `Streaming` here; disambiguation belongs to the
  journal/dispatcher and is out of scope (recorded limitation).

### Definition of Done

- **Unit tests:** tail turn ending on `tool_use` → `Streaming`; tail turn ending
  on `end_turn` → `Complete`; tail turn ending on `max_tokens` → `Complete`; a
  mid-tool tail (open `tool_use`, no result) → `Streaming`; a **balanced-tools**
  tail (all `tool_result`s present but last assistant `stop_reason` still
  `tool_use`, i.e. the 76-second-gap shape) → `Streaming` (this is the case the
  naive "open tools" heuristic misses — it is the highest-value test here); a
  non-tail turn ending on `tool_use` but followed by a new user prompt →
  `Complete`.
- **Classifier-shape tests (guard the positive-allowlist decision):** tail
  ending on `pause_turn` → `Streaming`; tail ending on an unknown/garbage
  `stop_reason` → `Streaming`. These fail under a "non-`tool_use` ⇒ terminal"
  implementation and are the regression guard against it.
- Existing disk-parser tests that assumed EOF-tail → `Complete` for terminal
  shapes still pass.
- **Docs corrected:** the `TurnStatus` docstring in
  `crates/harness/src/transcript.rs` currently states `Streaming` is live-only
  and that hydrated transcripts never carry it (and that truncated-mid-turn maps
  to `Failed`). M2 makes a hydrated Claude tail carry `Streaming`; update that
  doc to reflect the new rule and *why* (the `stop_reason` signal), so a future
  implementer doesn't "fix" hydrated `Streaming` back out.
- `harness-behavior.md` gap-register/behavior entry updated (Claude disk parse
  now derives tail-turn terminality from `stop_reason`).
- `make check` green.

---

## Milestone 3 — Completeness-ranked, idempotent hydrate merge

### Goal & Outcome

Change the frontend hydrate merge from first-write-wins to a deterministic
precedence so a complete parse supersedes a stranded non-terminal one of the
same identity, instead of being dropped by it.

Outcomes:
- When a slice already holds a non-terminal (`Streaming`) turn and a `Complete`
  turn with the same `hydration_key` is hydrated, the `Complete` turn wins and
  the stale partial is gone — the duplicate/stuck-spinner is resolved.
- When a slice already holds a turn and a *non-terminal* turn with the same
  identity arrives, the resident is kept (preserves the existing "a live or
  more-complete turn is not clobbered by a disk re-read" behavior).
- Live-only fields the disk parse lacks (e.g. context-window denominator,
  spend) are preserved when a disk turn supersedes a resident.

### Implementation Outline

- **In the `hydrate` reducer arm (`src/lib/state/reducers.ts:252–278`),** replace
  the `existingKeys.has(...)` drop-on-collision with an upsert keyed by
  `dedupKey` (`hydration_key ?? turn_id`):
  - No resident with that key → insert (as today).
  - Resident exists → **supersede only if** the incoming turn is terminal
    (`status` complete/failed) **and** the resident is non-terminal
    (`streaming`). Otherwise keep the resident. This single rule fixes Hole B
    while leaving the common "don't clobber the resident" path intact.
  - On supersession, **merge fields**: take the incoming turn's content/status,
    but fill any field the incoming leaves empty from the resident (the
    live-only enrichments). Do not blindly replace. **Identify the live-only set
    concretely:** every `Turn` field that `loadedTurnToTurn` (`reducers.ts:303–
    330`) does not populate from a `LoadedTurn`, plus fields it copies as empty
    because disk can't recover them (notably `usage.context_window`; `spend` is
    `None` from the Claude disk parser). Enumerate against the `Turn` shape so a
    field isn't silently dropped if the merge policy ever broadens.
  - *Scope note (severity is bounded today):* M3 only ever supersedes a
    **non-terminal** (`streaming`) resident, and the live-only enrichments
    (`spend`, final `usage`, `context_window`) are all stamped at turn *end* — a
    `streaming` resident usually hasn't received them yet, and a terminal live
    turn that *has* them is never superseded (the rule keeps it). So the field
    merge is mostly future-proofing, not a live data-loss fix — implement it for
    correctness, but don't contort a fixture to manufacture the case.
- Preserve ordering semantics the unified view relies on (the arm currently
  returns `[...fromDisk, ...turns]`); the upsert must not reorder surviving
  turns in a way that breaks the `buildUnifiedRows` sort. Determine the exact
  reuse against the code.
- Keep the `dedupKey` fallback to `turn_id` for keyless turns (user turns,
  keyless harnesses) — unchanged.
- Update the arm's doc-comment: the precedence rule and its rationale (why
  terminal supersedes non-terminal, why otherwise keep resident) must live in
  the code.

Load-bearing decision: precedence is computed from turn **properties**
(terminality), not arrival order. Arrival order was the Hole-B bug.

**Factor the precedence/merge into a named, reusable helper** (given two turns
sharing an identity, return the survivor with merged fields). M4's anchor-event
reconciliation calls the *same* helper to collapse resident-vs-resident
duplicates — establishing it here, in the first milestone that needs it, so M4
reuses rather than reinventing the policy.

Note the interaction with once-per-session hydration: within a *single*
reloaded session, hydration runs once (guarded by `hydrationAttempted` /
`hydrationStarted`), so M3's supersession fires when a *second* hydration of the
same agent occurs (e.g. a retry, an attach, or the next project reopen) — which
is exactly the path that produced the original duplicate. A reload that strands
a single partial with no second hydration will show a stuck spinner until the
next hydration; making that heal *live* is the deferred fix-c work, not this
milestone.

### Definition of Done

- **Reducer unit tests:** insert `Streaming` partial then hydrate `Complete`
  same-identity → one turn, the `Complete` one. Insert `Complete` then hydrate
  `Streaming` same-identity → resident `Complete` kept. Insert then re-hydrate
  identical `Complete` → idempotent (no duplicate, no churn). Keyless turns still
  fall back to `turn_id` and don't dedup across parses (documented existing
  behavior). **Field-preservation:** with a hand-built `streaming` resident
  carrying a live-only field (e.g. `usage.context_window` or `spend`),
  supersession by a `Complete` disk turn that lacks it preserves that field — a
  reducer-level test with a constructed resident, not a forced component fixture.
- **Component test (the wrapping component, per testing conventions):** simulate
  the reproducing sequence — hydrate a mid-flight partial, then a second
  hydration with the completed turn — and assert exactly one rendered agent row,
  complete, no stuck spinner. This is the end-to-end regression guard for the
  reported bug.
- **Switch-back accumulation test:** simulate repeated re-hydrations of the same
  in-flight turn (the `maybeRefreshProject` path), each catching a *later*
  intermediate state of the turn, and assert the agent row count stays at one
  (with M1's first-id identity these all share a key; without it they
  accumulated). This is the regression guard for the switch-induced multiplying.
- `make check` green (incl. eslint/svelte-check).

---

## Milestone 4 — Continuous live identity (stamp `hydration_key` at first assistant message)

### Goal & Outcome

M1–M3 collapse the switch-back *accumulation* (all disk re-merges now share the
first-id key). But a turn dispatched **live this session** carries no
`hydration_key` until `TurnEnd` (its key is `None`→`turn_id` while streaming), so
a switch-back disk re-merge can't dedup against it — leaving a single live-vs-disk
duplicate that does not self-heal (the merge dedups incoming-vs-resident, not
resident-vs-resident).

This milestone closes that **structurally**, not with a path-specific guard: make
the live turn carry its identity (`hydration_key` = first assistant message id)
**from the moment its first assistant message streams**, so live and disk turns
share a key continuously and the M3 upsert dedups them everywhere — the refresh
path, the per-agent hydrate path, project hydrate, and any future hydration path,
with no special-casing. This is the chosen approach over a refresh-only guard
because it fixes the actual defect (the live producer assigns identity only at
turn end) rather than papering over one of its symptoms, it is a structural match
rather than a runtime-state heuristic, and it removes the "live turn has no early
identity" limitation outright (see Known limitations, end of plan).

Outcomes:
- A turn dispatched and streaming live carries its `hydration_key` before it ends.
- Switching away from and back to a project whose agent has a live in-flight turn
  produces no disk duplicate beside the live turn, during the turn or after it
  completes.
- Project-open and post-reload behavior is unchanged.

### Implementation Outline

This builds directly on M1, which already has the live parser tracking the
turn's first non-subagent assistant `message.id` (to emit at `TurnEnd`). This
milestone gets that id to the reducer **early** and stamps it.

- **Live parser + event boundary.** Emit the turn's anchor id (the first
  non-subagent assistant `message.id`) on an **additive, content-free event**
  the first time it becomes known — once per turn, carrying `turn_id` and the
  anchor id. Route it through the `AdapterEvent → NormalizedEvent` boundary like
  the other live events. Additive by design: the wire enums are
  `#[non_exhaustive]` and the frontend reducer's default branch degrades
  gracefully, so an unknown/dropped variant falls back to M1's `TurnEnd` key —
  i.e. to pre-M4 behavior, never worse. (A new dedicated event is cleaner than
  bolting a `message_id` onto every `content_chunk`; the implementing agent
  picks the exact shape against the code.)
- **Reducer — stamp *and reconcile*, not just stamp.** A new arm, on the anchor
  event: (1) stamps `hydration_key` onto the live turn (found by `turn_id`) if
  unset; then (2) **collapses any other resident turn now sharing that key**,
  using the *same precedence/merge helper M3 introduces* (terminal supersedes
  non-terminal; otherwise keep the live/richer one; merge non-null fields). Step
  (2) is load-bearing — see the race note. Idempotent on re-delivery. The
  existing `turn_end` arm may keep stamping the same value as a harmless
  backstop; the early stamp is authoritative. **Reuse M3's helper — do not write
  a parallel collapse.**
- **No refresh-path guard is added.** The dedup now holds by identity, so the
  switch-back re-merge collides on a shared key and the reconciliation resolves
  it.
- **Protect the actively-streaming live turn from supersession (the new hazard
  M4 itself introduces).** Stamping a live turn's key *while it streams* makes it
  reachable by the hydrate arm's key-match branch — so a terminal disk re-read
  that lands in the brief window where disk shows the turn finished but the live
  `turn_end` hasn't been processed would *supersede the active live turn*,
  changing its `turn_id` and orphaning its remaining live events (dropped by the
  unknown-`turn_id` guards) — losing the turn-end-only enrichments (`spend`,
  final `usage`/context window) and reopening the race this milestone closes.
  M3's `resolveTurnCollision` alone cannot prevent this: by status it can't tell
  a *stranded disk partial* (safe to supersede) from an *active live turn* (must
  not be). M4 must therefore carry a **liveness/source signal** in state — mark a
  turn that is currently owned by a running dispatcher turn (`run_status ===
  "processing"` / a matching `in_flight_turn_id`) — and gate **both** the hydrate
  supersession and this reconciliation so an active live turn is never the loser.
  This is the one place the M3 helper is *not* a drop-in reuse; extend the gate,
  not the precedence rule.
- **Race note (why reconciliation is required — the stamp alone is not enough):**
  the live turn and the on-disk copy of the same turn become available at almost
  the same instant — both derive from the same first assistant message. A refresh
  can therefore read the disk turn (keyed by its first-id) and insert it *before*
  the frontend has processed the anchor event, while the live turn still has
  `hydration_key: undefined` (dedup falls back to its `turn_id`, so the two do
  **not** collide on insert). The anchor event then stamps the live turn — and
  now two *resident* turns share the key with nothing to collapse them, because
  the hydrate merge only dedups incoming-vs-resident. Step (2) above closes this
  by making the anchor event a resident-vs-resident reconciliation point, so the
  outcome is "exactly one row" regardless of which side won the race.

Sequencing: depends on M1 (the first-id value/tracking) and M3 (the upsert that
consumes the now-shared key).

### Definition of Done

- **Reducer unit test:** the anchor event stamps `hydration_key` on the matching
  live turn before `turn_end`; idempotent on re-delivery; a stray anchor for an
  unknown `turn_id` is a no-op.
- **Reconciliation/race test (the load-bearing one):** hydrate inserts a disk
  copy of the in-flight turn *before* the anchor event arrives (live turn still
  `hydration_key: undefined`, so two rows briefly coexist), then the anchor event
  fires — assert the row count collapses back to one, with the survivor chosen by
  M3 precedence (and live-only fields preserved if the live turn was the richer
  side).
- **Component/integration test:** an agent with a live streaming turn, then a
  switch-back refresh re-hydration of the same in-flight turn, yields exactly one
  rendered row; still one row after the turn completes and a further switch-back
  re-hydrates.
- **Active-live-turn protection test (guards the hazard M4 introduces):** a live
  streaming turn that has already been stamped with an early `hydration_key`,
  then a *terminal* disk re-read of the same key, must **not** supersede the live
  turn — the live turn (its `turn_id`) is kept, and a subsequent live `turn_end`
  still lands on it (not dropped as an unknown turn). This pins the liveness gate
  that distinguishes an active live turn from a stranded disk partial.
- **Graceful degradation test:** with the anchor event absent, behavior matches
  M1 (dedup at `TurnEnd`) — no regression.
- Project-open and reload paths unaffected (existing tests green).
- `make check` green.

## Known limitations (recorded, intentionally out of scope)

These are deliberate scope boundaries from the investigation; record them in
`docs/research/harness-behavior.md` so they are not mistaken for oversights:

1. **No live self-heal of a reload-stranded partial.** After a frontend reload
   mid-turn, the backend keeps running the turn but the reloaded frontend lost
   the dispatcher `turn_id`, so live events for it are dropped, and hydration is
   once-per-session. A stranded `Streaming` partial heals on the next hydration —
   which, given switch-back refresh (`maybeRefreshProject`), is typically the
   next switch-back after the turn completes, not live. (M4 does not help here:
   after a reload there is no live turn for the early-identity stamp to apply
   to.) The full fix is a dispatcher↔frontend resync that re-attaches live
   streaming to hydrated turns ("fix c").
2. **Disk can't distinguish "running" from "crashed/cancelled on a tool
   round."** Both present as a trailing `tool_use`; both hydrate as `Streaming`.
   A truly-abandoned tail turn renders a spinner that only resolves via
   journal/dispatcher reconciliation.

(The earlier "live turn exposes no identity until `TurnEnd`" limitation is now
**closed by Milestone 4**, not deferred.)

## Out-of-scope confirmations

- Cost/overage sidecar join (`stable_message_id`) is untouched.
- No new wire-format breaking changes beyond the additive `AdapterEvent::TurnEnd`
  identity field and the corrected `hydration_key` *value*.
- Non-Claude harness behavior is unchanged.
