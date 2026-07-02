# Transcript merge: prompt provenance (fix duplicated user prompts)

**Status:** Accepted
**Date:** 2026-06-21
**Branch:** `fix-transcript-merge-duplicate-prompt`

## Problem

### Symptom

In a long-lived agent's transcript, some of the user's own messages render
**twice** — once where expected, and once duplicated (often near the agent's
reply). Reported on the `switchboard-workflows` project, agent `coder`
(`019ece76-…`, Claude). The harness session file and the journal are each
internally consistent; the duplication is entirely in how Switchboard **merges**
the two on load.

### Root cause (verified)

Switchboard's conversation is a split source-of-truth (system-design §3): the
user's side comes from the **journal** (`journal.jsonl`, one `Send` record per
dispatch) and the agent's side from the **harness session file**. The user's
prompt appears in *both* (Claude records the prompt it received), so
`merge_project_conversation` (`crates/app/src/commands.rs:3693`) must render it
exactly once. It does this by **correlating harness turns to journal sends by
order/count**, front-anchored at the agent's first journaled send
(`#41`/commit `46250cc` reworked this; see the comment block at
`commands.rs:3806–3873`). The journal's `turn_id` is the dispatcher's, unrelated
to the harness file's ids, so there is no shared key to join on — hence counting.

The count assumes **every harness user-prompt turn corresponds to a Switchboard
send**. That is false. The Claude parser (`handle_user`,
`crates/harness/src/claude_code/session_file.rs:265`) emits a `Turn::User` for
*every* `type:"user"` string record, and Claude's file contains user-role string
records that are **not** Switchboard sends:

- `/compact` housekeeping — `<local-command-caveat>…`, `<command-name>/compact…`,
  `<local-command-stdout>…Compacted…`, and the `This session is being continued…`
  summary (one cluster per compaction).
- Prompts typed directly into the bare `claude` CLI rather than through
  Switchboard.

These extras are **interleaved throughout** the history, not just at the front,
so the front-anchored offset can't absorb them. Each interior extra shifts the
index pairing by one; downstream journaled prompts get misclassified as
"imported" and rendered a **second** time from the harness transcript.

**Verified against the real data** by running the actual
`merge_project_conversation` over the real journal + session file (a throwaway
`#[ignore]` diagnostic, not committed):

| measure | value |
| --- | --- |
| journal sends (coder) | 211 |
| rendered journaled user messages (`send_id = Some`) | 211 |
| rendered imported user messages (`send_id = None`) | 9 |
| imported copies that duplicate a journaled send (**the bug**) | **9** |
| harness user-prompt turns vs. sends | 238 vs. 211 (36 non-send extras) |

All 9 imported messages are duplicates of real sends (zero legitimate imported
content), and they cluster in the most recent turns — the signature of a drift
that accumulates over a compaction-heavy history. The reported message (the last
send) is among them.

> Note on "appears *after* the reply": in the finished-file state the duplicate
> sorts adjacent to its original (both precede the reply, since the harness
> prompt timestamp < the reply timestamp). It rendered *after* the reply for the
> reporter because they were viewing **while the turn was in flight**, where the
> in-flight classification positions it differently. The duplication is the
> defect; its exact position is timing-dependent.

### Why `#41` didn't close it

`#41` fixed the **front** offset (pre-journaling attached history) and the
**dangling in-flight** case, but its own comment concedes the interior case:
*"If an imported dangling prompt precedes a journaled one (a bare-CLI prompt,
then an in-flight Switchboard send)… the imported prompt is dropped and the
journaled one duplicated. Order alone can't disambiguate."* This plan closes
that residual.

### Why not content-matching

The obvious alternative — pair harness user turns to sends by **prompt text** —
is fragile and was rejected on evidence: of the 211 sends, **9 have harness text
≠ journal text**, and those 9 are exactly the **9 sends that carry attachments**
(the journal stores `prompt` and `attachments` separately; Claude's file records
the prompt with attachment text rendered in). Whitespace and future per-recipient
templating (M6) would add more divergence. Content-matching would mis-handle
precisely the attachment sends.

## Chosen approach

Stop inferring "is this a Switchboard send?" by counting, and read it from a
**structural provenance signal** Claude writes on each `type:"user"` record.
`promptSource` is a **4-value enum**, verified across the full local corpus (all
`~/.claude/projects/*/*.jsonl`):

| `promptSource` | meaning | corpus count | → provenance |
| --- | --- | --- | --- |
| `sdk` | dispatched by an SDK/headless client (Switchboard) | 1810 | `Sdk` |
| `typed` | typed into the bare `claude` TUI | 99 | `External` |
| `queued` | typed into the TUI while a turn ran | 2 | `External` |
| `system` | system-injected (e.g. a `<task-notification>`) | 1 | skip |
| *absent* (`None`) | older CLI with no marker | 3832 | `Unknown` |

For the reporting agent the `sdk` count matched the journal-send count exactly
(211 = 211), independent of text. **Note the large `None` population** — most
existing sessions predate the marker, so `Unknown` (→ fallback) is the common
path for historical transcripts, not an edge case.

Beyond `promptSource`, the file also contains `type:"user"` *string* records that
are **housekeeping, not prompts**, and must never become turns. Corpus-wide,
these slip past a naive "skip a few known prefixes" rule and would render as fake
user messages: `<command-message>` (×713), `<task-notification>` (×101+, incl.
some carrying `promptSource:"sdk"`), `<command-name>` (×132),
`<local-command-stdout>` (×100), `<local-command-stderr>` (×2), plus
`isCompactSummary` summaries and `isMeta` markers.

1. **Tag provenance in the Claude parser.** When `handle_user` sees a string
   prompt, classify it into `Sdk | External | Unknown`, carried as a new `source`
   field on `Turn::User` (`crates/harness/src/transcript.rs:32`). **Order is
   load-bearing — housekeeping skip runs BEFORE the `promptSource` mapping**,
   because some `<task-notification>` records carry `promptSource:"sdk"`; mapping
   first would let them become `Sdk` turns and consume send slots, re-introducing
   the exact drift this fix removes.

2. **Correlate by provenance in the merge.** Pair `Sdk` user turns to journal
   sends in order; **suppress an `Sdk` turn only when it actually consumes a send
   slot** (the journal renders that copy). An `Sdk` turn with no matching send
   (attached/imported history, or another SDK tool) renders **imported** — never
   dropped. `External` and any denylist-surviving `Unknown` turn always render
   imported. Agent-turn↔send pairing counts only slot-consuming `Sdk` turns, so
   interior housekeeping/bare-CLI turns never shift the alignment.

3. **Fallback when the signal is absent.** `Unknown` is a distinct state, not an
   alias of `External`. If an agent's user turns carry no positive provenance
   (all `Unknown` — a legacy file, or another harness), run today's
   `turn_offset`/`dangling_journaled` correlation unchanged — strictly no worse
   than current. (Mixed within one file: a denylist-surviving `Unknown` in a
   provenance-mode file renders imported, same as `External`. Rare in practice —
   modern files are all-`sdk`, legacy all-`None`.)

4. **Keep it harness-agnostic at the merge.** `source` is generic; only the
   Claude parser knows `promptSource`. Codex/Gemini/Antigravity leave it
   `Unknown` and ride the fallback; none exhibit the in-file interleaving today.

This **subsumes** `#41`'s `journal_start` timestamp boundary: pre-journaling
attached-session turns are not slot-consuming `Sdk`, so provenance renders them
imported without a timestamp hack. The dangling in-flight case is likewise
structural. `#41`'s logic is retained as the `Unknown` fallback, not deleted.

## Docs to read before implementing

- `docs/system-design.md` §3 (split source-of-truth) and §7 (sends vs. turns).
- `docs/harness-behavior.md` §3.1 — Claude session-file behavior; this
  adds the provenance/housekeeping shape (see Docs DoD below).
- The `#41` correlation comment (`crates/app/src/commands.rs:3806–3873`) and its
  `merge_*` tests — the contract being refined.
- The parser's record dispatch + skip list
  (`crates/harness/src/claude_code/session_file.rs:231–245`, `handle_user` `:265`).
- The off-wire serde precedent: `Turn::Agent.stable_message_id`
  (`transcript.rs:95`, `#[serde(default, skip_serializing)]`).

---

## Milestone 1 — Parser: classify and carry prompt provenance

### Goal & Outcome
Each `Turn::User` the Claude parser emits is tagged `Sdk | External | Unknown`,
and Claude housekeeping records never become turns.

### Implementation Outline
- Add `pub enum UserPromptSource { Sdk, External, Unknown }` with
  `#[derive(Default)]` and `#[default] Unknown`, in `transcript.rs`. Add a field
  `source: UserPromptSource` on `Turn::User`, annotated
  `#[serde(default, skip_serializing)]` — **backend-private, off the IPC wire**,
  matching the `stable_message_id` precedent. No `types.ts` change (the merge
  consumes `source` in-process; the frontend never sees it).
- In `handle_user`, in this order:
  1. **Housekeeping skip first** — drop (emit no turn) when `isCompactSummary` or
     `isMeta` is true, or content begins with a tag in the maintained denylist
     `{ <command-message>, <command-name>, <local-command-caveat>,
     <local-command-stdout>, <local-command-stderr>, <task-notification> }`.
     `log::debug!` + count what's skipped so future drift is visible, not silent.
  2. Then map `promptSource`: `"sdk"` → `Sdk`; `"typed"`/`"queued"` → `External`;
     `"system"` → skip (defensive — `system` should already be caught above);
     absent/unrecognized → `Unknown`.

### Definition of Done
- Unit tests in `session_file.rs`: `sdk` → `Sdk`; `typed`/`queued` → `External`;
  absent `promptSource` → `Unknown`; each denylist tag (esp. `<command-message>`
  and a `promptSource:"sdk"` `<task-notification>`) → **no turn emitted**; an
  `isCompactSummary`/`isMeta` record → no turn. **Negative test:** a genuine
  prompt whose text merely *contains* (not begins with) a wrapper-looking tag is
  preserved.
- Existing parser tests still pass (`queue-operation`/`last-prompt` skip
  unaffected).

---

## Milestone 2 — Merge: correlate by provenance, fall back when absent

### Goal & Outcome
`merge_project_conversation` renders each journaled send once; genuine bare-CLI
prompts render once as imported; housekeeping never appears; unpaired `Sdk`
prompts are never dropped. The real-data diagnostic drops from **9 → 0**.

### Implementation Outline
- In the per-agent loop (`commands.rs:3898–3977`), when the agent has positive
  provenance coverage, drive classification off `Turn::User.source`: pair `Sdk`
  turns to sends in order, suppress only slot-consuming ones; render `External`,
  `Unknown`, and unpaired `Sdk` as imported. Count only slot-consuming `Sdk`
  turns for agent-turn↔send `send_id` assignment.
- Gate on coverage: an agent whose user turns are all `Unknown` runs today's
  `turn_offset`/`dangling_journaled` path unchanged.

### Definition of Done
- Portable synthetic-fixture regression test reproducing the drift: journaled
  sends interleaved with a `/compact` cluster, a `<command-message>` **and** a
  `<task-notification>` record (the high-frequency corpus shapes), and a bare-CLI
  (`typed`) prompt → each journaled prompt once, the bare-CLI prompt once
  imported, **no housekeeping rows**.
- Regression test: `Sdk` prompt with an empty/short journal → prompt stays
  visible (unpaired `Sdk` not dropped).
- Re-run the real-data diagnostic locally (the `#[ignore]` harness): 9 → 0.
- `#41`'s existing `merge_*` tests still pass (the `Unknown` fallback preserves
  them).

---

## Milestone 3 — Docs

### Definition of Done
- `docs/harness-behavior.md` §3.1 — add a subsection documenting: the
  4-value `promptSource` enum (with the corpus counts above), the housekeeping
  `type:"user"` record family the parser skips, the large `None`/`Unknown`
  fallback population, and the new drift live-test. Note that the live
  `system/task_notification` *event* (G20 §4) and the on-disk `<task-notification>`
  *user-record* are related-but-distinct shapes.
- `docs/system-design.md` §3 — check for any "correlate harness turns to sends by
  order" wording; if present, update to "by provenance (Claude), order as
  fallback." The split-source *model* is unchanged.
- No `README` / `AGENTS.md` change (a fixed bug, not a standing user-facing
  limitation; imported bare-CLI prompts are expected behavior).

---

## Testing strategy

- **Parser unit tests** — provenance classification + housekeeping skip incl. the
  negative test (M1).
- **Merge regression tests** — the synthetic interleaving fixture (with
  `<command-message>` + `<task-notification>`) and the unpaired-`Sdk` test (M2),
  the committed proofs.
- **Real-data diagnostic** — the throwaway `#[ignore]` test over the reporter's
  actual files, run manually before/after (9 → 0). Not committed (hard-codes
  local paths); recorded here so it's reproducible.
- **Live test** (`crates/harness/tests/live.rs`, `live_claude_*`, `#[ignore]`) —
  dispatch one prompt and assert the written record carries `promptSource:"sdk"`,
  so a CLI version that renames/drops the field is caught by `make test-live`
  rather than silently regressing to the fallback.

## Risks & mitigations

- **Harness-format coupling.** We lean on Claude's internal session-file fields
  (`promptSource`, `isCompactSummary`, the housekeeping tags). Mitigation: the
  `Unknown` fallback degrades to today's behavior, never worse; the live test +
  the skip-counter surface a format change.
- **Mis-skipping a real prompt.** A genuine prompt that *begins* with a denylist
  tag would be dropped. Mitigation: match only on a closed denylist of known
  auto-generated wrapper tags (anchored at the start), plus the negative test in
  M1; accept the residual (a user opening a message with `<task-notification>` is
  vanishingly unlikely) over the larger risk of letting housekeeping through.
- **Denylist drift.** Claude could add a new housekeeping tag we don't skip.
  Mitigation: the skip-counter log makes new high-frequency `External`/`Unknown`
  shapes noticeable; revisit the denylist when the live test or a report flags it.

## Known limitations / out of scope

- **Straddling sessions ride the count fallback, not the full fix.** A session
  whose history spans the CLI version that introduced `promptSource` has both
  marked (`Sdk`) and unmarked (`Unknown`) prompts in its journaled region; since
  a journaled-region `Unknown` is a pre-marker dispatch the journal owns (not a
  bare-CLI prompt — those are `typed`/`queued`), provenance can't safely import
  it. Such agents fall back to the count path — **no worse than today** (the
  parser strips housekeeping either way), but they keep the continuation-drift
  rather than getting the full fix. The straddle set is a shrinking transitional
  artifact (every new agent is all-`Sdk`); fully fixing it would need per-span
  segmentation (count for the unmarked span, provenance for the marked span) and
  is deliberately deferred.
- **Hiding the `/compact` continuation summary** is a deliberate product choice
  (Claude-internal context). Surfacing compaction as a system marker is a
  separate change.
- **Other harnesses** keep today's behavior; revisit if Codex/Gemini grow
  equivalent in-file interleaving.
- **Retroactive de-duplication** needs nothing — the merge recomputes on every
  load, so the fix applies to existing transcripts immediately.
