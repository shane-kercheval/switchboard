# UI improvements

A focused UI/UX pass on the transcript, tool calls, Git view, sidebars, and compose bar. The
work is grouped into eight milestones ordered by dependency. Two of them (M2 tokens, M3 facets)
are foundations that every later milestone builds on; the rest are largely independent polish.

## Reading list — read before implementing

Project docs (read the first two in full; the others as referenced):

- `AGENTS.md` — build/test commands, coding conventions, test-type vocabulary, live-test policy.
- `docs/ui-conventions.md` — the token model, the `ui/` primitive set, theming. **M2 changes this
  document**; every other milestone must obey it.
- `docs/system-design.md` §3 (source-of-truth split), §7 (sends and turns), §9 (harness capability
  matrix).
- `docs/harness-behavior.md` — the operational source of truth for per-harness behavior. **M3 adds
  to it.**
- `docs/harness-update-review.md` — the playbook for probing a harness CLI. M3 step 1 follows it.

External docs:

- Tailwind v4 theme variables and `@theme inline`: <https://tailwindcss.com/docs/theme>
- Tailwind v4 colors / custom properties: <https://tailwindcss.com/docs/colors>
- Svelte 5 runes: <https://svelte.dev/docs/svelte/what-are-runes>
- `jsdiff` (`diff` npm package), specifically `structuredPatch`: <https://github.com/kpdecker/jsdiff>
- Vitest browser mode: <https://vitest.dev/guide/browser/>
- Codex CLI (for the `apply_patch` probe in M3): <https://github.com/openai/codex>

## Working agreement

**One commit per differentiable UI change.** Milestones group related work; each commit inside a
milestone is a self-contained, revertable go/no-go decision point. A commit that changes two
independent visual behaviors must be split. Per-commit granularity is specified in each milestone's
implementation outline. Do not commit this plan document.

**Rationale must survive into the code.** Several decisions below were reached by weighing
alternatives that are not recoverable from the codebase. Where a decision is non-obvious, carry the
*why* into a doc-comment or module comment at the site that depends on it. The plan is not the
durable artifact; the code is. Per `AGENTS.md`, do **not** reference milestones or this plan in code
comments — state the rule directly.

**No new abstraction without a second caller.** The one deliberate exception is `ToolFacet` (M3),
which is introduced with four callers on day one.

---

## M1 — Compose working-set correctness

Independent of everything else and shipped first because these are bugs, not taste questions. Two
related defects: the compose bar loses staged state on navigation, and it misreports the readiness of
a forward source that is still streaming.

### Goal & Outcome

Today, navigating away from a project — switching projects, or toggling to the Git view — silently
destroys most of what you have staged in the compose bar. Draft text, prompt-mode content, and
recipients survive (they are mirrored to `composeStore`); attachments, forward-from selections, and
workflow invocation state do not.

Separately, a forward-source chip for an agent that is *currently streaming* renders in the failed
status color with the caption "no output" — even though forwarding from a running agent is a
supported path that the send machinery explicitly handles by holding.

When this milestone is done:

- Attaching a file, switching to the Git view and back, and sending, works. The attachment is still
  there and still points at a file that exists.
- The same holds for a project switch, and for an app restart.
- "Forward from" selections survive navigation — both the message-level ones and the per-argument
  ones inside the prompt composer and the workflow composer.
- A half-filled workflow invocation (chosen workflow + field values) survives navigation.
- Everything above clears on a successful send/invoke, or when the user explicitly clears it.
- A staged-but-unsent attachment is no longer deleted out from under a live draft by the
  attachment garbage collector.
- A forward source that is still generating reads as *pending* — telling the user the send will hold
  for it — rather than as *failed*. The red warning is reserved for an agent that will actually be
  skipped from the send, and says so.

### Implementation Outline

**The diagnosis.** This is loss-by-unmount, not loss-by-reset. `App.svelte` wraps the compose bar in
`{#key selection.activeProjectId}`, so a project switch destroys the component; and the Git view is a
mutually-exclusive `{#if}` branch, so toggling to it destroys the component too. Every field held
only as component `$state` dies. `composeStore.ts` is why draft text and recipients survive.

**The chosen approach, and what it was chosen over.** We keep the `{#key}` remount and make
`composeStore` the source of truth for *all* compose state, rather than hoisting the ComposeBar above
the view switch to keep it permanently mounted. The remount is an existing, well-understood,
already-tested reset boundary; removing it is a larger refactor of `App.svelte` with no additional
benefit once the store holds everything. If the store is correct, remounting is free.

**The trap.** `gc_unreferenced_attachments` in `crates/app/src/commands.rs` deletes every file in a
project's attachments directory that is not referenced by a journal `Send` record, and it runs from
`load_project_conversation_impl` — i.e. on every project load. Persisting attachment chips without
touching the GC produces chips that dangle at paths the GC just reclaimed. The existing comment at
`ComposeBar.svelte` (the `AttachmentChip` block) documents exactly this, and is why chips are
session-only today. **The GC fix must land before or with the chip persistence, never after.**

The fix: `load_project_conversation_impl` takes an additional parameter — the set of attachment paths
referenced by the caller's live draft — and unions it into the `referenced` set passed to
`gc_unreferenced_attachments`. The frontend already knows these paths (they are in the persisted
compose snapshot) and passes them at load time. No new file format, no new persistence layer on the
Rust side, and the GC keeps reclaiming genuinely orphaned files. Record the reason for the parameter
in a doc-comment on the command: *a draft is durable UI state the backend cannot see, so the caller
must declare its live references.*

**Store shape.** Bump `STORAGE_VERSION` in `composeStore.ts` and extend `ComposeSnapshot`. The
existing v2→v3 migration path (parse, degrade malformed entries to an empty plain draft) is the
pattern to follow — an unknown or malformed extension field degrades to absent, never throws. The
snapshot gains:

- staged attachments (the wire `Attachment` fields; the chip's local `id` can be regenerated on
  restore),
- message-level forward sources (`ForwardSource[]` from `heldForwards.svelte.ts`),
- per-argument forward sources for prompt mode, and per-field forward sources for workflow mode,
- workflow invocation state: the workflow identity (`name` + `is_builtin`) and its field values.

**Restore-time validation.** Forward sources name agents. An agent may have been removed from the
roster since the draft was written. `ComposeBar` already prunes stale agents from the restored
recipient selection — restored forward sources go through the same prune. A restored attachment whose
path no longer exists is dropped with no error; the draft is ergonomic, not load-bearing.

**In-flight staging across a remount.** `stage_attachment` is async, and the ComposeBar currently
bumps a generation counter on unmount to abandon in-flight staging results. Once the store is the
source of truth, a staging result must be committed to the store **keyed by the project it began
under**, regardless of whether that project's ComposeBar is still mounted. The generation guard is
then only needed for the send-clear race (a result landing after the chips were committed and
cleared). Make this explicit in the code — it is the single subtlest part of this milestone.

**Persistence contract.** `composeStore`'s existing contract — mutations synchronous, only
persistence debounced, serialize at fire time not schedule time — must hold for the new fields. Do
not introduce a second write path.

### Forward-source readiness — the second defect

`ComposeBar.svelte`'s `agentHasCompletedOutput` tests `turn.role === "agent" && turn.status ===
"complete"`. A turn that is in flight has `status: "streaming"` (see `src/lib/state/types.ts`), so the
predicate returns `false` for a streaming agent, and `ui/ForwardSourceChip.svelte` renders its `empty`
branch — `border-status-failed/40 bg-status-failed-soft/40 text-status-failed`, captioned "no output",
tooltipped "This agent has no completed output to forward."

That is the *failed* status token, shown at the moment the feature is about to work. The chip does not
merely fail to describe what will happen — it asserts the opposite. Forwarding from a running agent is
first-class: per `forward_message_impl` in `crates/app/src/commands.rs`, a forward "holds outside any
queue while each `source` agent's current in-flight turn settles, then composes … each non-empty
source's latest completed output."

Read that contract carefully; it defines all three states and leaves nothing to guess:

- **Ready** — the agent is idle and has at least one completed turn. The forward resolves it
  immediately. Neutral chip (today's non-empty styling).
- **Pending** — the agent has an in-flight turn. **This holds whether or not it also has an older
  completed turn**, because the forward always awaits the in-flight turn and then takes the *latest*
  completed output — so a streaming agent with prior history forwards the new turn, not the old one.
  The send will hold. This is informational, not a warning: no status-failed treatment. Caption to the
  effect of "still generating," and `status-processing` is the natural token, matching how the agent's
  own run state renders elsewhere. **This is the state the bug report is about.**
- **Empty** — the agent is idle and has no completed turn (including one whose only turn `failed` or
  `cancelled`). Per `ForwardOutcome`, such a source is *skipped* from the composed body, and if
  **every** source is empty the forward is `Invalidated` and the composer restores. Keep the warning
  treatment — it is earned here.

The `Empty` caption is currently "no output," which describes the agent's state rather than the
consequence to the user. "Will be skipped" is the more useful phrasing, since that is literally what
`ForwardOutcome::Resolved { skipped }` does with it. Take that copy improvement in the same commit.

Note what this means for `Ready` vs `Pending`: readiness is **not** "has a completed turn." It is "has
a completed turn *and* nothing in flight." An implementation that only adds an `is_streaming` check on
top of the existing predicate will get the both-states case backwards. Record the
awaits-in-flight-then-takes-latest rule in a doc-comment on the derivation, because it is the entire
semantics of the chip and it lives in a Rust file the frontend author will not naturally read.

**Consolidate the predicate.** The same "does this agent have output" question is asked in four
places today — `ComposeBar`'s chip row, `ComposeBar`'s `@`-menu rows ("no output yet"),
`ui/ForwardSourcePicker`, and via the `empty` prop on `ui/ForwardSourceChip`, with `PromptComposer`
and `WorkflowComposer` passing it through. Move the tri-state derivation into
`heldForwards.svelte.ts`, next to `ForwardSource`, so all four surfaces cannot disagree.

`ForwardSourceChip`'s boolean `empty` prop becomes a three-valued state. That is a breaking change to
a `ui/` primitive with a handful of call sites, and it is the correct one — a boolean cannot express
this domain, which is how the bug happened.

**Sequencing note.** This lands *after* the forward-source persistence commit, since both touch the
same `forwardSources` code in `ComposeBar` and the persistence change is the more invasive of the two.

**Commits:**

1. Backend: `load_project_conversation_impl` accepts live-draft attachment paths and excludes them
   from the GC. (Ships alone; no visible behavior change, but unblocks commit 3.)
2. `composeStore` schema v3 + migration, with the new fields defined but not yet written by
   ComposeBar.
3. Attachments persisted; frontend passes draft paths to the load command.
4. Forward sources persisted (message-level, prompt per-argument, workflow per-field).
5. Workflow invocation state persisted.
6. Tri-state forward-source readiness derivation in `heldForwards.svelte.ts`; all four call sites
   adopt it. `ForwardSourceChip`'s `empty` boolean becomes a state.
7. Pending-state visual treatment on the chip and the picker rows.

Commits 6 and 7 are split so the derivation change (which alters *which* chips are flagged) is
revertable independently of the visual treatment (which alters *how* they are flagged).

### Definition of Done

- Rust unit tests: `gc_unreferenced_attachments` retains a file that is in the live-draft set and
  absent from the journal; still deletes a file in neither; still deletes nothing when the directory
  is missing.
- Frontend unit tests on `composeStore`: v2 blob migrates to v3 without losing draft/recipients; a
  v3 blob with a malformed attachments array degrades to no attachments rather than throwing; an
  unknown extension field round-trips or is dropped without error.
- **Component-level tests on `ComposeBar`** (per `AGENTS.md`: pure-reducer tests are insufficient for
  components wrapping IPC + subscriptions). Mock `invoke`/`listen`. Cover: attach → unmount →
  remount restores the chip; attach → send → remount shows no chip; a staging result that resolves
  *after* unmount lands in the originating project's snapshot; a staging result that resolves after
  send-clear is discarded; a restored forward source naming a removed agent is pruned.
- Unit tests on the tri-state readiness derivation: an idle agent with a completed turn → ready; an
  agent with only a streaming turn → pending; an agent with a completed turn *and* a newer streaming
  one → **pending** (the forward awaits the in-flight turn — this is the case a naive
  `hasCompleted || isStreaming` predicate gets wrong, so name the test after the rule); an agent with
  no turns → empty; an agent whose only turn `failed` → empty; same for `cancelled`.
- Component tests: a chip for a streaming source does not carry the failed styling; a chip for a
  genuinely empty source still does; the picker rows and the `@`-menu rows agree with the chip for the
  same agent — that last one is the regression test for the four-call-sites divergence.
- Manual verification of both reported bugs: (a) attach an image, press ⌘⇧G to the Git view, come
  back, send — then repeat across a project switch and an app restart. (b) Start a turn on one agent,
  add it as a forward source while it is still streaming, and confirm the chip reads as pending
  rather than red.
- Known limitation to record in the `composeStore` module comment: attachments persist across
  restart, but the staged file lives under `.switchboard/`, which the user may clean; a restored
  chip whose file has vanished is dropped silently.

---

## M2 — Design-token foundation

Every subsequent styling milestone (M4, M5, M7, M8) depends on this. It lands early so nothing gets
styled twice.

### Goal & Outcome

The app has roughly seven near-identical neutral fills in light mode (`#ffffff`, `#fafafa`,
`#f8f8f9`, `#f4f4f5`, `#f3f3f5`, `#ebebee`, plus `#e4e4e7` — the *border* token — used as a fill),
multiplied by five opacity modifiers into ~15 effective shades. Several differ by one or two percent
luminance. That is the root cause of the "everything is gray" feeling: the steps are too small to
read as hierarchy but numerous enough to look muddy. `ui-conventions.md` says "build depth by
stepping the layers"; the layers stopped stepping.

When this milestone is done:

- The neutral ramp is three fills and one line, with each having exactly one job.
- Hovering a row or an icon button uses a token that means *hover*, not the border color.
- Focusing the compose textarea shows a visible ring, and blue means exactly one thing in the app.
- A future component cannot silently reintroduce a fifteenth gray — CI catches it.
- `docs/ui-conventions.md` describes the ramp that actually exists.

### Implementation Outline

**The ramp.** Four neutral roles, each with a single job:

| Token | Job |
| --- | --- |
| `surface` | app chrome — sidebars, title bar, the field everything sits on |
| `raised` | content — the reading surface, cards, popovers |
| `panel` | recessed / inset — code blocks, inputs, expanded tool output |
| `border` | lines only, never a fill |

Plus new `hover` and `active` interaction tokens, and a new `focus` token.

**Two rules to enforce, both mechanically checkable:**

1. **No opacity modifiers on surface tokens.** `bg-panel/35` composes differently over every parent
   and produces shades nobody named. Ban `bg-{surface,panel,raised,border}/<n>`.
2. **`border` is never a fill.** Ban `bg-border` in any form. The ~dozen `hover:bg-border/60` sites
   across `DiffPanel`, `GitRepoNode`, `TranscriptPanes`, `SettingsView`, `Sidebar`,
   `ProjectsSidebar`, `ui/AsyncIconButton`, `ui/CopyButton` become `hover:bg-hover`.

A third rule is a code-review rule rather than a lint, because it needs a human eye: **no more than
two nested neutral treatments, counting fills and borders together.** A bordered container's child
gets a fill or nothing, not both. Write this into `ui-conventions.md` as the durable statement.

**Enforcement.** Add a frontend test that scans `src/` for the two banned patterns and fails with the
offending file/line. This is cheap, runs in the default `make test`, and is the only thing that stops
the ramp re-accreting. Keep it to those two mechanical rules; do not attempt to lint the nesting rule.

**The focus token.** `ui-conventions.md` currently designates `accent` (a teal) for focus rings. A
green ring on a text field reads as *valid*, not *focused*. Introduce a `focus` token in blue,
and — because blue must then mean one thing — convert the two raw-hue violations at
`UnifiedTranscript.svelte` (`bg-blue-100/20`, on the user bubble and the held-forward bubble) to a
named token in the same commit family. These are the only palette-hue violations in `src/`.

The compose ring appears on **actual textarea focus**, not permanently. The compose bar is the
default keyboard target, so an always-on ring carries no information; a ring that disappears when
focus moves into the Git view's keyboard-nav mode or a dialog is a real signal.

**Sequencing is load-bearing.** Introduce tokens before rewriting call sites; land the lint last, or
it fails the intermediate commits.

**Commits:**

1. Add `hover` / `active` / `focus` tokens to `app.css` (both light and dark mappings); no call-site
   changes.
2. Replace every `hover:bg-border/*` and `bg-border` fill with `hover:bg-hover` / `bg-active`.
3. Collapse the redundant neutral values (`code-bg`, `code-bg-agent`, `code-bg-user`, and the
   `surface`/`panel` near-duplicates) onto the four-role ramp; remove opacity modifiers from surface
   tokens at every call site.
4. Name the user-bubble blue; convert the two `bg-blue-100/20` sites.
5. Compose-bar focus ring.
6. The CI scan test for the two banned patterns.
7. Rewrite the "neutral surfaces" section of `docs/ui-conventions.md` to state the four roles, the
   two banned patterns, and the two-nested-treatments rule.

### Definition of Done

- The scan test exists, fails on a deliberately introduced `bg-panel/50`, and passes on `main`.
- `make check` is green (this includes `svelte-check` and the browser suite).
- Visual verification in both light and dark mode of: both sidebars, the Git view at all three panes,
  a transcript with tool calls, the compose bar focused and unfocused, an open dropdown, an open
  dialog. Dark mode is where a collapsed ramp most easily goes wrong.
- `docs/ui-conventions.md` updated. The rationale ("steps too small to read as hierarchy") belongs in
  that doc, not in `app.css`.
- Known limitation: the two-nested-treatments rule is unenforced by tooling and relies on review.
  Say so in `ui-conventions.md`.

---

## M3 — Tool facets

The one backend milestone. M4 and M5 both sit on it.

### Goal & Outcome

Today a tool call reaches the frontend as `{ name: String, input: serde_json::Value }`, with the
input never inspected by anything in Rust. For Claude, Gemini, and Antigravity this is rich: a Claude
`Edit` call's `{file_path, old_string, new_string}` and a `TodoWrite`'s `{todos: [...]}` are sitting
there, being pretty-printed as JSON. **Codex is the exception**: every builtin arrives as
`name: "command_execution"` with `input` = the argv, and its file edits ride inside `apply_patch` as a
shell heredoc. You cannot distinguish "edit a file" from "run the tests" without parsing the command.

A frontend `switch (tool.name)` would render Claude beautifully and leave Codex as a JSON blob
forever, while baking four harness vocabularies into Svelte components. Instead, each adapter maps
its own vocabulary once, in Rust, where it is testable — and the frontend renders a normalized shape.

When this milestone is done:

- Every tool call carries a normalized `facet` describing *what kind of operation it is*, alongside
  the untouched raw `name` and `input`.
- A Codex `apply_patch` is recognized as a file edit, with per-file before/after content, exactly as
  a Claude `Edit` is.
- A Codex shell command and a Claude `Bash` call produce the same facet.
- The frontend can render a stable, scannable verb for every tool call from every harness.
- Unmapped tools degrade to a generic facet rather than an error.

### Implementation Outline

**Step 1 is a probe, not code.** We do not have a recorded fixture for Codex `apply_patch`, nor for
Claude `Edit` / `Write` / `TodoWrite` (grep confirms: no `old_string` / `new_string` / `todos` appears
anywhere under `crates/harness`). Following `docs/harness-update-review.md`: run each harness live
against a scratch directory, capture the raw stream and session-file records for an edit, a
multi-file edit, a file write, a file read, a shell command, a search, and (Claude) a todo update.
Record them as fixtures under `crates/harness/tests/fixtures/<harness>/`. **Write the observed shapes
into `docs/harness-behavior.md` before writing any parser** — an interrupted session must not lose
the probe results. Only then implement the mappings.

The exact wire shape of Codex's `apply_patch` is the single largest unknown in this plan. Do not
guess it from training data. If the probe reveals that Codex emits a dedicated patch event rather
than (or in addition to) a `command_execution`, prefer that event and record why.

**The facet type.** Lives in `crates/harness/src/events.rs` alongside `ToolKind`. `#[non_exhaustive]`,
serde-tagged like the surrounding types, so a new variant is additive across IPC.

```rust
pub enum ToolFacet {
    Edit { files: Vec<EditedFile> },
    Write { path: String, content: String, truncated: bool },
    Read { path: String },
    Shell { command: String, cwd: Option<String> },
    Search { pattern: String, path: Option<String> },
    Todo { items: Vec<TodoItem> },
    Mcp { server: String, tool: String },
    Other,
}

pub struct EditedFile {
    pub path: String,
    pub change: EditChange,          // Added | Modified | Deleted
    pub edits: Vec<EditPair>,        // one per Claude `Edit`; N per `MultiEdit` / `apply_patch`
    pub truncated: bool,
}

pub struct EditPair { pub old: String, pub new: String }
```

Contract notes that must survive into the doc-comments:

- `Edit` carries a *list of files* because Codex's `apply_patch` can touch several in one call, and a
  list of pairs per file because Claude's `MultiEdit` makes several changes to one file. Claude's
  `Edit` is the degenerate case: one file, one pair.
- `Write` is distinct from `Edit` even though a write is arguably an edit with an empty `old`: the
  harness gives us the new content but *not* the prior content, so we cannot honestly render a diff.
  The renderer says "wrote file" and shows content, not a diff.
- `Shell` does not carry an exit code. The facet is computed at `ToolStarted`, before the tool has
  run; failure is already carried by the existing `is_error` on `ToolCompleted`. Do not duplicate it.
- Edit/write content is capped (choose a bound in the low hundreds of KB) with a `truncated` flag, so
  a single enormous write cannot bloat a live event. The raw `input` is unchanged and remains the
  escape hatch.
- `Other` is the graceful-degradation variant for any tool we have not mapped, including Claude's
  `Task` (subagent dispatch), which is **deliberately not given a facet in this pass** — it renders
  via the generic path. Adding it later is additive.

**Where classification happens.** One function per harness, `classify(name, input) -> ToolFacet`,
living in that harness's module. Each harness has **two** call sites — the stream parser and the
session-file parser — and they must produce identical facets for the same logical tool call. Put the
classifier in one place per harness and call it from both; do not inline the mapping twice. The
Claude reload path is `claude_code/session_file.rs`; the live path is `parser.rs`. Codex's are
`codex/session_file.rs` and `codex/parser.rs`.

**Where the facet travels.** Two structs, because live and reload are separate paths into the same
frontend type:

- `NormalizedEvent::ToolStarted` / `AdapterEvent::ToolStarted` in `crates/harness/src/events.rs`
- `TurnItem::Tool` in `crates/harness/src/transcript.rs`

Both gain a `facet` field. On the frontend, `ToolCall` in `src/lib/state/types.ts` gains `facet`, and
the `tool_started` case in `src/lib/state/reducers.ts` carries it through. `input` and `name` stay
exactly as they are — the facet is additive, and the raw values remain the provenance escape hatch
that the M4 row design depends on.

**Codex `apply_patch`.** Reconstruct `old`/`new` strings per file from the patch's context and
+/- lines, so the frontend has one uniform `Edit` renderer rather than a second patch-shaped one. An
"Add File" section yields `old: ""` and `change: Added`; a "Delete File" section yields `new: ""` and
`change: Deleted`. Note in the code that we deliberately normalize *into* before/after strings rather
than *out to* hunks, because the diff is computed lazily on the frontend (M4) and a single
representation keeps one renderer.

**Line numbers.** Neither Claude's `Edit` input nor Codex's `apply_patch` carries absolute file line
numbers (Codex uses context-anchored `@@` headers, not `@@ -a,b +c,d @@`). Facets therefore carry no
line numbers, and M4's diff is snippet-scoped. This is a deliberate accepted limitation: you are
reading the change, not navigating to it. Record it in the facet's doc-comment.

**Commits:**

1. Fixtures + `docs/harness-behavior.md` update from the probe. No code.
2. `ToolFacet` type, wired into both event structs and `TurnItem::Tool`; Claude classifier; TS type
   mirror; reducer passthrough. Facet unused by any renderer yet.
3. Codex classifier, including the `apply_patch` parser.
4. Gemini + Antigravity classifiers.

### Definition of Done

- **Unit tests** per harness classifier, driven by the recorded fixtures: each of Edit, MultiEdit,
  Write, Read, Bash/exec, Grep/search, TodoWrite maps to the expected facet; an unknown tool name maps
  to `Other`; an MCP tool maps to `Mcp` with the server and tool split correctly.
- **Unit tests on the `apply_patch` parser** specifically, since it is the only real parsing in this
  milestone: single-file modify; multi-file; add file; delete file; a patch whose body contains a line
  that looks like a patch delimiter; a malformed patch (degrades to `Shell`, does not panic).
- **Equivalence test**: for each harness, the stream parser and the session-file parser produce the
  same facet from the same recorded tool call. This is the test that catches the two-call-site
  divergence the design is guarding against.
- **Live tests**, per the `AGENTS.md` naming convention (`live_<harness>_…`, harness name first, or
  the test silently drops out of `make test-live-<harness>`): `live_claude_edit_emits_edit_facet`,
  `live_codex_apply_patch_emits_edit_facet`. These are the tests that notice when a CLI vendor changes
  the shape upstream. This is an adapter-touching change, so per `AGENTS.md` it must land with live
  coverage.
- Truncation: a write larger than the cap sets `truncated` and does not blow up the event.
- `docs/harness-behavior.md` records the observed tool shapes per harness, and the gap register notes
  that Codex has no per-builtin tool name.

---

## M4 — Tool-call row redesign

### Goal & Outcome

A tool call today is a bordered, filled disclosure containing a bordered `bg-panel/60` INPUT block of
pretty-printed JSON and an OUTPUT block. Eight tool calls in a turn means eight borders and sixteen
fills wrapped around eight lines of shell commands. The Codex INPUT block additionally shows
transport noise (`max_output_tokens`, `yield_time_ms`) nobody wants.

Nothing is hidden or summarized. The user explicitly wants every tool call and intermediate step
visible — that is the product. What changes is the chrome, not the count.

When this milestone is done:

- A collapsed tool call is a borderless, fill-less row: an icon, a bold normalized verb, a muted
  provenance detail, and a status glyph.
- A run of tool calls reads as a set, held together by the icon column rather than by boxes.
- Expanding a tool call reveals its content on exactly one recessed fill — chrome appears on demand.
- A file edit renders as an actual diff of what that one tool call changed, not the file's current
  state.
- A shell call renders its command and output; a read renders a path; a todo update renders a
  checklist.
- The raw `name` and `input` JSON remain reachable behind expansion for every tool call.

### Implementation Outline

**The row.** Left icon (facet-derived) · bold verb in `text-fg` · muted detail, ellipsis-truncated,
never wrapping · right-aligned chevron and status glyph. No border, no fill, when collapsed.

**Verb vocabulary.** The bold column only scans as a column if the verbs are a fixed, small set — and
the verb must encode *state*, not just facet, so a running tool reads differently from a finished
one. Define the vocabulary in one place (facet × state → label), not inline per component:

- `Shell` → `Running command` / `Command run` / `Command failed`
- `Edit` → `Editing file` / `File edited` (plural when `files.len() > 1`)
- `Write` → `Writing file` / `File written`
- `Read` → `Reading file` / `File read`
- `Search` → `Searching` / `Searched`
- `Todo` → `Updating todos` / `Todos updated`
- `Mcp` → the server/tool pair
- `Other` → the raw tool name, as today

**The muted detail is provenance.** `Bash: git log --oneline -3`, and for Codex
`exec_command: git log --oneline -3`. It is how you know what actually ran under the normalized verb.
Keep it; truncate it; expansion shows it in full.

**Status is the only color.** A quiet check on success — keep it, because with the chrome gone the row
has no other way to say it finished — and `status-failed` on failure. This reverses an earlier
inclination to drop the success glyph; the reason it survives is that it is now the *only* completion
signal, which is worth a comment at the site.

**Chrome on expansion, one level only.** Collapsed: nothing. Expanded: the output/diff sits on a
single `panel` fill and nothing inside it gets a second fill. This is the M2 two-nested-treatments
rule falling out naturally, and it is what keeps a forty-line shell output from bleeding into the
agent's prose. Without it the borderless design fails.

**The Edit renderer and the diff.** `DiffView.svelte` is purely presentational — it takes
`{ diff: FileDiff, style, language }` and fetches nothing. `DiffPanel.svelte` is the thing that
fetches libgit2 working-tree diffs, and is **not** the reuse target: it would show the file's current
state, not what this tool call did. Reuse `DiffView` with a synthesized `FileDiff`.

Synthesize it on the frontend, lazily, only when a row is expanded:

- Add the `diff` npm package (`jsdiff`) via `pnpm add diff` — there is no diff algorithm on the
  frontend today. Per `AGENTS.md`, use the CLI, never hand-edit `package.json`; commit the lockfile
  with it.
- Map `structuredPatch(old, new)` output onto the existing `FileDiff` / `DiffHunk` / `DiffLine`
  types in `src/lib/types.ts`. No type changes needed.
- Line numbers are **snippet-relative**, because the facet carries no absolute offsets (see M3). Say
  so in the hunk header rather than presenting relative numbers as if they were file positions.

The alternative — computing hunks in Rust and shipping them — was rejected: it does the work eagerly
for rows nobody expands, it doubles the wire payload, and `DiffHunk` lives in `crates/git`, which
`crates/harness` has no business depending on. Record that reasoning where the synthesis happens.

**Multi-file edits** (Codex `apply_patch`) render one diff section per `EditedFile`.

**Commits:**

1. Borderless row shell + verb vocabulary + status glyph + expansion-only chrome. All facets still
   render the generic JSON body when expanded. This commit alone should make the transcript feel
   different, and is independently revertable.
2. `Shell` renderer (command + output).
3. `Edit` renderer with inline diff (`pnpm add diff` lands here).
4. `Write` / `Read` / `Search` / `Todo` renderers.
5. `Mcp` renderer.

Commit 1 before the per-facet renderers is deliberate: it isolates the visual change from the content
changes, so a "no-go" on the row design does not also revert the renderers.

### Definition of Done

- Component tests on the tool row: collapsed row shows verb + detail + status for each facet; a long
  command truncates rather than wrapping; expanding shows raw `name` and `input` for every facet
  including specialized ones; a failed tool shows the failed status; a cancelled/stopped tool (the
  frontend-synthesized `stop_reason` in `types.ts`) still renders.
- Unit tests on the diff synthesis: a one-line change; a change with no trailing newline; an addition
  (`old` empty); a deletion (`new` empty); a `truncated` facet renders the truncation notice; a
  multi-file `Edit` renders one section per file.
- Unit tests on the verb vocabulary: every `ToolFacet` variant × {running, done, failed} yields a
  label; an unknown facet discriminant (forward-compat, `#[non_exhaustive]`) degrades to the raw tool
  name rather than rendering blank. This is the reducer-default-branch discipline `AGENTS.md`
  requires for additively-evolving wire enums.
- Visual verification against a real Claude turn and a real Codex turn, both with edits.
- No browser test needed — nothing here is layout-measurement-coupled.

---

## M5 — Per-turn changed-files card

### Goal & Outcome

A card at the end of a turn listing the files that agent edited during the turn, grouped by
directory, with `+n / −n` counts.

Crucially this is derived from the turn's `Edit` / `Write` facets, **not** from git. Switchboard's
premise is N agents working concurrently in one directory; a before/after git snapshot around a turn
would capture other agents' edits and your own editor's edits, and could attribute none of them. The
card's honest claim is *"files this agent edited via tools"* — attributable and precise — rather than
*"files that changed"* — complete but unattributable. Record this in the card's module comment,
because it is the kind of decision a later contributor will otherwise "fix."

The accepted limitation, which must also be recorded: an agent that edits via `sed -i`, `git apply`,
or `npm install` changes files without an edit tool, and those edits will not appear on the card.

When this milestone is done:

- After a turn that edited files, a compact card lists them, grouped by directory, with change counts.
- Clicking a filename scrolls the transcript to that file's tool call and highlights it.
- A small icon beside each filename opens the Git view showing that file's *current* full diff.
- The icon is absent when the project's directory is not a git repository.

### Implementation Outline

**Two affordances, two meanings — this is the point, not redundancy.** The filename answers *what did
this agent do to this file, in this turn*: attributable, historical, exact. The icon answers *what
does this file look like right now*: complete, current, includes other agents' and your own edits,
unattributable. Neither substitutes for the other. Label the icon so the difference is legible (a
tooltip along the lines of "View current diff in Git"), and do not use a bare arrow.

A third affordance, "open in editor," already exists as the `editor_command` preference and is
**deliberately not added here** — three per-file affordances is one too many.

**Rollup.** An agent may edit one file five times in a turn. The card lists it once, with counts
summed across all of that turn's edits to it, and clicking navigates to the first.

**Counts** come from the facet's `EditPair`s (count added/removed lines between `old` and `new`), not
from git. A `Write` contributes all-added. A `truncated` edit contributes a count marked approximate.

**Git-view navigation.** The Git view is repo-scoped. Opening it for a file requires resolving the
file to a repo root and setting `gitView.svelte.ts`'s `diffTarget` to the uncommitted-changes target
for that worktree — the same state the Git view already uses to select a diff. This is real plumbing
but not new machinery. A project directory need not be a git repo; in that case omit the icon rather
than rendering it broken.

**Commits:**

1. The card: derive from facets, group by directory, counts. Rendered, inert.
2. Filename click → scroll to and highlight the tool call.
3. Git icon → open Git view at that file's current diff.

### Definition of Done

- Unit tests on the derivation: a turn with no edits yields no card; five edits to one file roll up
  to one row with summed counts; a multi-file `apply_patch` yields one row per file; a `Write`
  contributes all-added; edits from *other* agents' turns never appear on this agent's card.
- Component tests: clicking a filename scrolls to and highlights the right tool call when the same
  file was edited by several calls (it targets the first); the Git icon is absent in a non-git project.
- Manual verification of the multi-agent case: two agents editing the same file in overlapping turns
  produce two cards, each listing only its own agent's edits.
- The attribution rationale and the shell-edit blind spot are recorded in the card's module comment.

---

## M6 — Resize primitive and persisted layout

Independent of M2–M5; can be built in parallel.

### Goal & Outcome

Neither sidebar is resizable — `ui/SidebarPanel.svelte` takes a Tailwind class as its width prop and
applies `shrink-0`. Sidebar collapse state exists but is plain `$state` in `App.svelte`, so it does
not survive reload. Meanwhile resize logic is hand-rolled three separate times: `TranscriptPanes`
(fraction-based, per-project, persisted, clamped — the good one), `GitView.detailWidth`, and
`DiffPanel.fileListWidth` (both pixel-based and reset on every *mount*, not merely every reload).

When this milestone is done:

- Both sidebars can be dragged to a new width, with sensible minimums and maximums.
- Double-clicking a resize handle resets it to its default width.
- Sidebar widths and collapse state survive an app restart.
- The Git view's detail-pane width and the diff panel's file-list width survive navigation and restart.
- There is one resize implementation, not four.

### Implementation Outline

**Extract, don't reinvent.** `TranscriptPanes` + `transcriptPanes.svelte.ts` is already a complete,
tested model: fraction-based so it restores proportionally across monitor sizes, min-width clamped,
drafted locally during the drag and committed on pointer-up. Extract the drag mechanics into a
`ui/` primitive and reuse it. Add double-click-to-reset to the primitive so every consumer inherits it.

**Scope of persistence — a decision that cannot be recovered from the code.** Sidebar widths and
collapse state are **global**, in `localStorage`, not per-project. A sidebar's width expresses a fact
about your monitor and your reading preference; it means the same thing in every project. Transcript
pane *fractions* are per-project because pane *membership* is per-project — the layout means something
different in each. Making sidebars per-project would reflow the whole app on every project switch for
zero gain. Follow the `theme.svelte.ts` / `agentCopy.svelte.ts` model, and clamp the restored width
against the current viewport on read (a width saved on a 32" monitor must not consume a 13" laptop).

The `ui-conventions.md` note on why theme lives in `localStorage` rather than `config.yaml` applies
verbatim here: this is a device-local appearance preference, and syncing it via a git-tracked file
would be wrong. Carry that reasoning into the new store's module comment.

**Sequencing.** Adopt the primitive in the *new* consumers first (sidebars, Git view, diff panel), and
refactor `TranscriptPanes` onto it **last**, in its own commit, guarded by its existing tests. If that
last refactor turns out to be risky, it is the one commit to drop — leaving `TranscriptPanes` on its
own logic is a smaller sin than destabilizing a tested, load-bearing layout.

`SidebarPanel`'s `width` prop is a Tailwind class today. It becomes a number; that is a breaking
change to the primitive with two call sites, and it is the right one.

**Commits:**

1. `ui/` resize primitive (drag, min/max clamp, double-click reset). No consumers yet.
2. Sidebars resizable; widths and collapse state persisted globally, clamped on read.
3. `GitView.detailWidth` and `DiffPanel.fileListWidth` adopt the primitive and persist.
4. `TranscriptPanes` adopts the primitive. (Droppable if risky.)

### Definition of Done

- Unit tests on the persistence store: a width beyond the viewport clamps on read; a corrupt blob
  degrades to defaults; collapse state round-trips.
- Component tests on the primitive: drag adjusts width; clamps at min and max; double-click resets;
  pointer-up commits exactly once.
- **A browser test** for the viewport clamp, since it is geometry-measurement-coupled and jsdom cannot
  exercise it. Per `AGENTS.md`, poll measured geometry (`expect.poll`), never a fixed sleep.
- `TranscriptPanes`' existing tests pass unchanged after commit 4 — that is the whole safety argument
  for doing it last.

---

## M7 — Git view

Depends on M2.

### Goal & Outcome

The Git view is where the gray is most overloaded, but the three-pane structure is correct and stays:
the changed-files column may hold a handful of files on a small commit and dozens on a real branch,
and the diff is the whole point of the view. The problems are nested neutrals and missing signal.

When this milestone is done:

- No surface in the Git view stacks three neutral treatments.
- The diff reads as the primary canvas rather than as another gray sidebar.
- Each changed file shows `+n / −n` counts.
- A commit's subject is legible at a glance against its timestamp.

### Implementation Outline

**Three concrete gray stacks to unwind**, all of which the M2 ramp makes expressible:

- `GitRepoNode.svelte`: `bg-surface` repo list → `bg-raised` repo card → `bg-panel` branches drawer.
  Three neutral layers; drop the drawer's fill for a left rule.
- `DiffPanel.svelte`: `bg-raised` diff pane → `bg-panel` file-list column → `bg-surface` list header.
  A gray header inside a gray drawer. The file list becomes `raised` with a border; the header loses
  its fill.
- `GitView.svelte`: the repo list is `bg-surface` — chrome color — inside the white content pane.
  Content is `raised`.

**The commit list — the fix is the inverse of what it looks like.** In `GitRepoNode.svelte`'s
`commitList` snippet, the row button carries `text-muted` when unselected, the timestamp span carries
`text-muted font-mono text-[11px]`, and the subject span *inherits* `text-muted` from the row. The
timestamp is already correctly recessed. Nothing is promoted, which is why the list reads as a wall of
uniform gray. Give the subject `text-fg`. Leave the timestamp exactly as it is — it is minimal,
correctly styled, and load-bearing context for identifying a commit.

**Explicitly out of scope**: moving the changed-files column into the diff pane header (the pane earns
its width), and reworking the `Show branches without folders` checkbox into a filter menu (raised in
discussion, never affirmed — leave it alone).

**Commits:**

1. Unwind the three gray stacks.
2. `+n / −n` counts per changed file.
3. Commit subject promoted to `text-fg`.

### Definition of Done

- Visual verification in light and dark of: a repo with the branches drawer open; a diff with a long
  file list; a selected commit; an uncommitted-changes selection; a binary file; a too-large file
  (both existing `FileDiff` placeholder paths must still read correctly against the new surfaces).
- The M2 scan test stays green.
- Existing Git-view tests pass; add a test for the counts derivation only if it involves logic beyond
  reading `FileDiff`'s existing structure.

---

## M8 — Agents sidebar and transcript polish

Depends on M2. Last because it is the most taste-driven and the least coupled.

### Goal & Outcome

The agents sidebar is a stack of four monospace `key: value` rows per agent — `model: opus`,
`effort: high`, `mcp: 2`, `skills: 5` — which reads as a debug dump. Everything has equal weight, and
the things that change over time (status, context) are buried or absent. Agent names truncate to
ambiguity: two different agents both render as `gpt-5-5-mi…`. That is a bug, not a nit.

When this milestone is done:

- Two agents with similar names are always distinguishable in the sidebar.
- An agent's run status is the most visually salient thing on its card.
- Model and effort read as one line of secondary text; MCP and skills counts are compact chips.
- The context bar survives unchanged — it is the best thing on the card.
- Transcript message actions (copy, expand, forward) are quiet until hover or focus.

### Implementation Outline

**Truncation.** Middle-truncate, wrap to two lines, or tooltip — the implementing agent should pick
against the real layout. The requirement is only that two agents whose names share a long prefix are
distinguishable without hovering. Verify with a roster containing `gpt-5-5-minimal` and
`gpt-5-5-minimal-2`, which is the exact case that fails today.

**Status.** Use the existing `status-*` tokens and the `StatusDot` primitive per `ui-conventions.md`
(pass `label` only when the dot is the sole signal). A colored left rule on the card is the strongest
expression; the dot is the fallback if the rule fights the sidebar's border.

**Density.** `opus · high` as one secondary line. MCP and skills as icon+count chips. Keep the context
bar as-is.

**Message actions.** They sit at full opacity on every turn today. Quiet them to hover/focus. Focus
visibility is non-negotiable — a keyboard user must still be able to reach and see them, so this is
`opacity` on hover *and* `:focus-visible`, not `display: none`.

**Commits:**

1. Agent name truncation fix.
2. Agent card restructure (status prominence, model·effort line, mcp/skills chips).
3. Transcript message actions quiet until hover/focus.

### Definition of Done

- Component test: a roster with two long shared-prefix names renders two distinguishable labels.
- Component test: message actions are present in the accessibility tree at all times and become
  visible on `:focus-visible` — the keyboard path must not regress.
- Visual verification with agents in each status (idle, processing, failed, cancelled), light and dark.

---

## Cross-milestone summary

| Milestone | Depends on | Backend? | Rough shape |
| --- | --- | --- | --- |
| M1 compose correctness | — | yes (GC param) | bug fixes |
| M2 token foundation | — | no | foundation |
| M3 tool facets | — | yes | foundation |
| M4 tool-call rows | M2, M3 | no | the headline change |
| M5 changed-files card | M4 | no | new surface |
| M6 resize + layout | — | no | primitive + persistence |
| M7 Git view | M2 | no | polish |
| M8 sidebar + transcript | M2 | no | polish |

M1, M2, M3, and M6 have no dependencies on each other and could proceed in parallel if that is
useful. M4 is the milestone the whole plan is pointed at.

Run `make check` before opening the PR; it includes the browser suite, which `make test` does not.
Adapter-touching work (M3) additionally requires `make test-live-claude` and `make test-live-codex`.
