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

**Commit strategy is out of this plan's scope.** The plan describes *what to build*, not how to slice
commits. Implement a milestone, run it through a review cycle, then commit — one commit per milestone
by default, with additional commits only for follow-up passes (a visual-tuning round, say). Do not
commit this plan document.

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
existing unversioned→v2 migration path (`migrateUnversioned`: parse, degrade malformed entries to an
empty plain draft) is the
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
`ForwardOutcome::Resolved { skipped }` does with it. Take that copy improvement as part of the same change.

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

**Sequencing note.** Do the forward-source persistence work *before* this, since both touch the same
`forwardSources` code in `ComposeBar` and the persistence change is the more invasive of the two.

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
2. **`border` is never a fill.** Ban `bg-border` in any form. `hover:bg-border/60` appears **31 times
   across 13 files** — do not work from an enumerated list, `rg 'hover:bg-border' src/` and migrate
   every hit to `hover:bg-hover`. Two of those hits are in **test files**
   (`DiffPanel.test.ts`, `GitRepoNode.test.ts`), which assert the class string directly and will fail
   if missed. Start with the shared primitives (`ui/iconButton.ts`, `ui/AsyncIconButton`,
   `ui/CopyButton`) — migrating those fixes several consumers at once — then re-grep.

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
named token in the same pass. These are the only palette-hue violations in `src/`.

The compose ring appears on **actual textarea focus**, not permanently. The compose bar is the
default keyboard target, so an always-on ring carries no information; a ring that disappears when
focus moves into the Git view's keyboard-nav mode or a dialog is a real signal.

**Sequencing is load-bearing.** Introduce tokens before rewriting call sites, and add the scan test
last — it would fail against the intermediate states while call sites are still being migrated.

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
`name: "command_execution"` whose `input` carries a single shell-command string (e.g.
`"/bin/zsh -lc ls"`, not an argv array), and its file edits ride inside `apply_patch` as a
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
    /// Absolute, normalized. See the path contract below.
    pub path: String,
    pub change: EditChange,          // Added | Modified | Deleted
    pub edits: Vec<EditPair>,        // one per Claude `Edit`; N per `MultiEdit` / `apply_patch`
    pub truncated: bool,
}

pub struct EditPair { pub old: String, pub new: String }
```

**The path contract — define this before anything consumes it.** Every `path` on every facet
(`EditedFile`, `Write`, `Read`, `Search`) is an **absolute, normalized filesystem path**. Each adapter
is responsible for resolving its harness's spelling against the agent's working directory before
constructing the facet: harnesses do not agree on absoluteness, and Codex's `apply_patch` section
paths are relative to the shell's cwd.

Carry exactly one path field, not two. The frontend derives a project-relative *display* path at
render time; a second wire field would be redundant state that can disagree with the first.

This matters because M5 groups the turn's edits by file and resolves each to a git worktree. Without
a normalization rule, the same file arriving under two spellings becomes two rows on the card, and a
path outside the project silently gets a Git affordance that cannot work. **Add to the probe checklist
below: record the observed absoluteness of edit/read/write paths for each harness in
`harness-behavior.md`.** The contract does not depend on what the probe finds — the adapters normalize
regardless — but the probe tells each adapter how much work normalizing is.

Contract notes that must survive into the doc-comments:

- `Edit` carries a *list of files* because Codex's `apply_patch` can touch several in one call, and a
  list of pairs per file because Claude's `MultiEdit` makes several changes to one file. Claude's
  `Edit` is the degenerate case: one file, one pair.
- `Write` is distinct from `Edit` even though a write is arguably an edit with an empty `old`: the
  harness gives us the new content but *not* the prior content, so we cannot honestly render a diff.
  The renderer says "wrote file" and shows content, not a diff.
- `Shell` does not carry an exit code. The facet is computed at `ToolStarted`, before the tool has
  run; failure is already carried by the existing `is_error` on `ToolCompleted`. Do not duplicate it.
- Edit/write content is capped (choose a bound in the low hundreds of KB) with a `truncated` flag.
  **Be precise about what this buys:** it prevents the facet from *duplicating* a large payload. It
  does **not** bound the event, because the raw `input` rides alongside, uncapped, exactly as it does
  today. Bounding the raw input is out of scope; bounding what the *renderer* does with it is M4's job.
  The raw `input` is unchanged and remains the escape hatch.
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

**Collapsed rows must be cheap — this is a real bug in the code being replaced.** Today
`ui/Disclosure.svelte` is a native `<details>` whose body is rendered unconditionally
(`{@render children()}`, outside any `{#if open}`), and `ToolCallWidget.svelte` derives `hasInput` from
`formattedInput`, which is `JSON.stringify(input, null, 2)`. Because `hasInput` is read in the
always-rendered body, **every tool call in the transcript formats its full raw input regardless of
whether it is expanded.** A 500 KB `Write` builds a 500 KB string and a DOM node nobody asked for.
Collapsing hides it and costs the same.

The new row owns its own expansion state rather than delegating to `<details>`, so:

- format and render the raw-input body only when the row is actually open, and
- cap the *displayed* raw JSON with an explicit "truncated" affordance.

This is not a bonus optimization; it is the reason M3's facet cap does not, on its own, bound anything.

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

### Definition of Done

- Component tests on the tool row: collapsed row shows verb + detail + status for each facet; a long
  command truncates rather than wrapping; expanding shows raw `name` and `input` for every facet
  including specialized ones; a failed tool shows the failed status; a cancelled/stopped tool (the
  frontend-synthesized `stop_reason` in `types.ts`) still renders.
- **Regression test for the eager-stringify bug**: a tool call whose `input` is multi-megabyte does
  not produce the formatted string while collapsed. Assert on behavior (the body node is absent, or
  `formatToolInput` is not called), not on internals. A separate test that an over-cap raw input
  renders the truncation affordance when expanded.
- Unit tests on the diff synthesis: a one-line change; a change with no trailing newline; an addition
  (`old` empty); a deletion (`new` empty); a `truncated` facet renders the truncation notice; a
  multi-file `Edit` renders one section per file.
- Unit tests on the verb vocabulary: every `ToolFacet` variant × {running, done, failed} yields a
  label; an unknown facet discriminant (forward-compat, `#[non_exhaustive]`) degrades to the raw tool
  name rather than rendering blank. This is the reducer-default-branch discipline `AGENTS.md`
  requires for additively-evolving wire enums.
- Visual verification against a real Claude turn and a real Codex turn, both with edits.
- No browser test needed — nothing here is layout-measurement-coupled.

### As-built decisions (recorded at implementation review)

Resolved during implementation, confirmed by the engineer; recorded here so the plan matches what
shipped:

- **Verb vocabulary revised after visual review — supersedes the facet × state table above.**
  Labels are state-invariant nouns: `Command`, `Edit`, `Write`, `Read`, `Search`, `Todos`, the
  server/tool pair for MCP, the raw name for the generic facet. The status glyph (spinner / quiet
  check / failed / cancelled) is the row's sole state signal; encoding state into the verb
  duplicated it, and a noun also reads correctly for a tool cancelled mid-flight.
- **Detail is facet-derived, not a raw-input preview.** The verb already names the operation, so
  prefixing the raw tool name (`Bash:`, `run_command:`, `file_change:`) was pure duplication. The
  detail is the facet's substance — the command line (display-redacted via `redactDisplay`), the
  file path(s), the pattern, a todo summary — with the input preview kept only for MCP/generic
  facets. The raw tool name moved into the expanded raw-input section's label, so provenance stays
  reachable.
- **`ThinkingWidget` and the compaction marker restyled to the same row grammar** (icon · label ·
  muted preview · chevron; lazy body behind the left rule) so every transcript collapsible shares
  one visual language. The compaction marker became `CompactionMarker.svelte`, which left
  `ui/Disclosure` with zero consumers — it is deleted.
- **Tool-row diffs are always unified**, ignoring the user's `diff_style` preference: side-by-side
  needs a 48rem minimum width, which cannot fit the row's 600px content cap without horizontal
  scrolling. The Git view still honors the preference.
- **Content-less Codex edits get a placeholder.** This section predates M3's finding that a live
  Codex `file_change` announces paths without content (content arrives via the turn-end facet
  upgrade, and in a rare correlation-mismatch case never arrives). An Edit facet with empty `edits`
  renders the path plus "Diff will appear when the turn completes" while the turn streams, or "Diff
  content unavailable" on a settled turn — `ToolCallWidget` takes a `turnSettled` prop for the
  distinction.
- **Expanded-body composition (revised after a second visual pass — the wrapping `panel` fill is
  gone).** A slab per open row made a run of expanded tools wall-to-wall gray, and its first line
  duplicated the row's still-visible detail. Instead the expanded content hangs under the row
  behind a thin left rule (the fan-out column idiom), directly on the reading surface, and the
  row's detail line hides while open since the body shows the full untruncated version. Fills mark
  only true content blocks: the output / raw-JSON `pre`s on `panel` (that token's documented job),
  file changes in a bordered diff canvas. For specialized facets the raw JSON sits
  behind a "Show raw input" reveal (the facet body already shows the same information readably);
  the generic facet has no body, so its raw input shows directly.
- **Edit diffs render inline, without expansion** (third visual pass): watching the changes
  stream by is the point of the row, so an Edit facet's per-file diff sections are always visible
  under the row; expansion reveals only output and raw input, and the edit row's detail line is
  suppressed (the per-file headers carry the paths). Eager rendering is safe here — edit content
  is capped at the facet level and off-window rows aren't mounted. The Codex placeholder shows
  inline too and swaps to the real diff automatically when the facet upgrade lands at turn end.
  A single-file edit reads by its change kind — `added` → **Write**, `deleted` → **Delete** —
  because harnesses without a separate write tool (Codex) create files via patch; multi-file
  patches keep **Edit** with per-file markers. Tool-row diffs render through a new `compact` mode
  on `DiffView` (no hunk-header bars, no line-number gutters — snippet-relative numbers read as
  file positions they aren't; hunks separated by a hairline). The Git view is untouched.
- **Write facets use the same inline diff treatment as added files.** A dedicated write and a patch
  that creates a file represent the same user-visible change, so both render as an all-added compact
  diff with the same collapsed preview and full captured content on expansion. Output and raw input
  remain behind expansion; a facet-level truncation still surfaces DiffView's notice.
- **Inline edit and write previews show 25 lines per file.** Forty lines made a single tool call
  dominate the transcript before the user chose to expand it; 25 preserves enough context to scan
  while keeping the surrounding conversation visible.
- The old "TOOL"/"MCP"/"Plugin" kind label and `Badge` are gone from the row; the facet icon
  (lucide) plus verb replace them. The raw-JSON display cap is 50 k characters.
- New modules: `src/lib/toolRow.ts` (facet × state verb vocabulary, provenance detail, icon map)
  and `src/lib/toolDiff.ts` (jsdiff `structuredPatch` → `FileDiff` synthesis; snippet-relative
  hunk headers carry an explicit qualifier). Dependency added: `diff` (jsdiff v9, bundled types).

---

## M5 — Per-turn changed-files card

**Status: SKIPPED (decided 2026-07-11, after M4 shipped) — superseded by M4's as-built inline
diffs.** When this section was designed, tool rows hid their content behind expansion, so the card
was the only at-a-glance view of a turn's edits. M4's final form renders every edit's diff inline
in the transcript, attributed to its agent, which removed the card's primary value; the Git view
already covers "this file's current diff" one keystroke away. The remaining unique value —
per-agent aggregated `+n/−n` counts and jump-to-a-specific-edit navigation — was judged not worth
a new derivation module plus the Git-view initial-file state channel (the riskiest piece, touching
`DiffPanel`'s tested load effect). Nothing here was foundational: M6–M8 depend only on M2, and
everything the card would consume (Edit/Write facets, `EditPair` counts) shipped in M3/M4 and
stays — so this can be revived as specced if aggregated per-agent summaries are missed in real
use. The long-turn "jump to a message/edit" need moves to M9's scoping pass, which addresses
transcript navigation generally. The spec below is retained for that possible revival.

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

**Git-view navigation needs a state channel that does not exist yet.** The Git view is repo-scoped:
`gitView.svelte.ts`'s `diffTarget` selects a repo/worktree or a commit, and carries **no concept of a
file**. Which file is shown is private state inside `DiffPanel.svelte`, chosen *after* the file list
loads — it keeps the previous selection if still present, otherwise falls to `files[0]`. Setting
`diffTarget` alone therefore opens the Git view to the right worktree and displays the wrong file
whenever the requested one is not first.

So: extend the `diffTarget` variants with an optional initial file (repo-relative), and have
`DiffPanel`'s load effect prefer it. Three constraints, all load-bearing:

- **Apply the initial file only when the target key actually changed**, not on a same-target refresh.
  `DiffPanel` already discriminates on `filesKey !== key`; reuse it. Otherwise a background refresh
  yanks the user's current file selection back to the one the card requested minutes ago.
- **Fall back to today's behavior when the requested file is absent from the change list** — the agent
  edited it, but it may since have been committed or reverted. Show the first file, not an empty pane.
- The path arrives from the facet as absolute (M3's contract) and must be converted to repo-relative
  *after* resolving it under a tracked worktree. A path that resolves outside any tracked worktree
  gets **no icon** — an agent can edit a file anywhere it has access, which is not a bug, and the card
  must not offer a Git affordance that cannot work. Same for a project directory that is not a repo.

### Definition of Done

- Unit tests on the derivation: a turn with no edits yields no card; five edits to one file roll up
  to one row with summed counts; a multi-file `apply_patch` yields one row per file; a `Write`
  contributes all-added; edits from *other* agents' turns never appear on this agent's card.
- Unit tests on path normalization at the card boundary: the same file arriving under two spellings
  yields **one** row, not two; a path outside any tracked worktree yields no Git icon.
- Component tests: clicking a filename scrolls to and highlights the right tool call when the same
  file was edited by several calls (it targets the first); the Git icon is absent in a non-git project.
- Component tests on the initial-file channel: requesting the **second** file in the change list opens
  that file's diff, not the first; a same-target refresh preserves the user's manual file selection
  rather than reapplying the requested one; a requested file absent from the change list falls back to
  the first file without error.
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
refactor `TranscriptPanes` onto it **last**, guarded by its existing tests. If that last refactor turns
out to be risky, it is the piece to drop — leaving `TranscriptPanes` on its own logic is a smaller sin
than destabilizing a tested, load-bearing layout.

`SidebarPanel`'s `width` prop is a Tailwind class today. It becomes a number; that is a breaking
change to the primitive with two call sites, and it is the right one.

### Definition of Done

- Unit tests on the persistence store: a width beyond the viewport clamps on read; a corrupt blob
  degrades to defaults; collapse state round-trips.
- Component tests on the primitive: drag adjusts width; clamps at min and max; double-click resets;
  pointer-up commits exactly once.
- **A browser test** for the viewport clamp, since it is geometry-measurement-coupled and jsdom cannot
  exercise it. Per `AGENTS.md`, poll measured geometry (`expect.poll`), never a fixed sleep.
- `TranscriptPanes`' existing tests pass unchanged after that refactor — that is the whole safety
  argument for doing it last.

### As-built decisions (recorded at implementation review)

- **The primitive is the drag interaction, not a width model.** `ui/ResizeHandle.svelte` owns the
  handle element, the pointer lifecycle, min/max clamping (inverted range → midpoint, generalizing
  the pane row's too-narrow behavior), draft-vs-commit callbacks, and double-click reset. Consumers
  own the geometry mapping: pixels for sidebars / Git view / diff panel; `TranscriptPanes` converts
  the gutter's pixel value to fractions in its callbacks, so the fraction model is untouched.
- **Window listeners, not pointer capture.** The browser drag tests (and the repo's established
  resize idiom) dispatch `pointermove` on `window`; `setPointerCapture` would also throw on
  synthetic pointerdowns. An armed-drag guard gives "commits exactly once" instead.
- **`value` and `max` are thunks** read at drag time: the Git detail pane's start value can be a
  *measured* CSS default (`w-2/3`) rather than a stored number, and maxima are live container
  fractions.
- **One store, one key.** `src/lib/layout.svelte.ts` (sibling of `theme.svelte.ts`) holds both
  sidebars (`{width, open}`), `gitDetailWidth` (null = never dragged; reset returns to null and
  the CSS default), and `diffFileListWidth` under a versioned envelope. Widths clamp on read
  *and* write; sidebar max = min(480px, 40% viewport) floored at the 200px minimum.
- **Three `SidebarPanel` call sites, not two** — the plan predates the agents-sidebar loading
  shell (`App.svelte`), which shares the persisted agents width so project load doesn't jump.
- **Collapse persistence is intent, not visibility.** Only the open/closed booleans persist;
  App's derived gating (projects sidebar hidden in Git view / without content) is unchanged.
- **Sidebar handles are hover-highlighted overlay strips** on the inner edge (no layout change,
  current visuals preserved); Git view / diff panel handles keep their existing gutter styling.
- **Preserved side effects:** GitView's and DiffPanel's window `pointermove` listeners doubled as
  the keyboard-nav hover-suppression release; both survive as dedicated listeners. GitView's
  expand-toggle now clears the drag draft (the old code cleared the resize flag).
- **Pane gutter double-click equalizes the adjacent pair** — the natural "default" for a boundary
  in a fraction model. `MIN_PANE_WIDTH_PX` stays where it was; the Git detail minimum moved into
  the store (`GIT_DETAIL_MIN_WIDTH`), removing the mirrored-constant comment's other half.

Post-review fixes (two AI reviewers, three confirmed findings):

- **Live viewport bound is CSS, not a resize listener.** The store's clamp only ran at
  read/write, so a mid-session window shrink left an oversized panel squeezing the content area.
  Each pixel consumer now mirrors its clamp as a `max-width`
  (`clamp(200px,40vw,480px)` on `SidebarPanel`, `85%` on the Git detail aside — the *actual*
  container, tighter than the store's viewport approximation — `clamp(176px,55%,440px)` on the
  diff file list). Deliberately **not** a store-rewriting resize listener: the stored preference
  survives a transient shrink and re-expands when the window grows.
- **Drags and keyboard steps start from the rendered width.** `ResizeHandle` clamps its start
  value to `[min, max()]`, so an adjustment under a CSS cap begins from what's on screen, never
  the invisible stored value (no first-pixel jump).
- **Keyboard path (WAI-ARIA window splitter).** The handle is focusable with
  `aria-valuenow/min/max`; arrow keys draft ±16px steps and commit once on key release — the
  pointer's draft/commit-once contract transposed, so a held key doesn't write localStorage at
  key-repeat rate.
- **Interrupted drags finalize.** `pointercancel` and window blur commit what's on screen and
  disarm, closing the stuck-drag class (armed handle resizing on bare pointer motion after a
  Cmd-Tab). No new `onCancel` API; Escape-to-revert deliberately skipped until a real need.

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

### Definition of Done

- Visual verification in light and dark of: a repo with the branches drawer open; a diff with a long
  file list; a selected commit; an uncommitted-changes selection; a binary file; a too-large file
  (both existing `FileDiff` placeholder paths must still read correctly against the new surfaces).
- The M2 scan test stays green.
- Existing Git-view tests pass; add a test for the counts derivation only if it involves logic beyond
  reading `FileDiff`'s existing structure.

### As-built decisions (recorded at implementation review)

- **The `+n/−n` counts were a backend feature, not a frontend derivation.** The plan implied counts
  might fall out of `FileDiff`; they can't — `FileDiff` loads lazily for the selected file only.
  `ChangedFile` (git crate + TS mirror) gained `additions`/`deletions` as `Option<u32>`: `None` for
  binary or oversized content (the `git diff --numstat` `-` placeholder), computed per delta via
  libgit2 line stats. The commit path counts on the tree diff it already holds; the worktree path
  keeps its status-walk enumeration (it owns staged/unstaged/untracked semantics) and joins counts
  from a parallel workdir diff keyed by path — any counts failure degrades to `None`, never fails
  the listing. `max_size` on both diffs keeps huge blobs from being loaded just to count lines. A
  rename keys by its new path and, via `find_similar`, shows the real edit size.
- **Counts render right-aligned in quiet mono** (green `+n` / red `−n` on the `diff-*` tokens),
  inside the row button so the hover padding shift slides them clear of the revealed action icons.
  Hidden when both are zero — a pure rename's `R` badge already says everything.
- **Commit list previews 15 commits with a "Show N more" row** (user-requested during review of the
  live view: one branch's 50-commit read buried every other repo card). Expansion is keyed to the
  loaded ref, so selecting another branch re-collapses; a partially-hidden range's "…older commits
  not shown" note is superseded by the Show-more row; keyboard nav walks only rendered commits.
- **Row hovers moved `bg-raised` → `bg-hover`** in the drawer and file list: their surfaces are now
  raised (or transparent on raised), where `raised` hover is invisible — the exact pairing
  `app.css`'s `.md-code-copy` comment documents.
- **File paths dimmed a step** (`text-muted/80` → `/60`): the filename is what's scanned; the
  repeated directory prefix was competing with it.
- **Post-review measurements and fixes**: the parallel-diff counts cost was benchmarked with tree
  size and changed-count varied independently (the duplicated tree walk and the per-file patch
  cost are separate terms); worst case measured ~84ms at a 20k-file tree with 300 changes —
  accepted, recorded qualitatively in `worktree_line_counts`'s comment. The two rename detectors'
  silent coupling got a comment plus a heavy-edit rename test (deliberately above the similarity
  threshold, not at it).
- **Live-view iteration (user-driven)**: the preview cap applies to the `recent` range only —
  `incoming` (what a pull brings) always renders in full; the Show-more row lives inside the
  history section. Resize-handle hover unified on the focus blue across all six handles (idle
  looks stay per-context). Repo cards flattened to sections — border/rounding/fill dropped, the
  header row anchors and branches/commits hang on left rules; list padding tightened.

---

## M8 — Agents sidebar and live-turn indicators

Depends on M2. Last because it is the most taste-driven and the least coupled.

### Goal & Outcome

Two unrelated surfaces, grouped because both are small and neither blocks anything.

The agents sidebar is a stack of four monospace `key: value` rows per agent — `model: opus`,
`effort: high`, `mcp: 2`, `skills: 5` — which reads as a debug dump. Everything has equal weight, and
the things that change over time (status, context) are buried or absent. Agent names truncate to
ambiguity: two different agents both render as `gpt-5-5-mi…`. That is a bug, not a nit.

Separately, live turns announce themselves by pulsing their own label text (`animate-pulse` on the
words `Working...` and `Queued...`). The text you are trying to read is the element that fades, and
nothing tells you how long a turn has been running.

When this milestone is done:

- Two agents with similar names are always distinguishable in the sidebar.
- An agent's run status is the most visually salient thing on its card.
- Model and effort read as one line of secondary text; MCP and skills counts are compact chips.
- The context bar survives unchanged — it is the best thing on the card.
- Live turns show a standard animated loading indicator; the label text itself never animates.
- A running turn shows how long it has been running; a completed turn shows how long it took.

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

### Live-turn indicators

**One animation everywhere. The label and the number do the differentiating.** Do not encode state in
the animation's color, tempo, or presence — that was considered and rejected as over-design. Queued,
running, and no-response are all live states and all animate identically.

Add `ui/LoadingDots.svelte`: three `<span>`s with a staggered opacity keyframe — the standard
three-dot loader. A 1.4 s loop, per-dot delays of `0s` / `0.16s` / `0.32s`, opacity `0.2 → 1 → 0.2`.
It must be a component with real spans, **not** an animated `…` text character: animating a glyph
means swapping characters, which shifts layout. The dots inherit `currentColor` so they take the
label's color for free. Under `prefers-reduced-motion` all three sit at full opacity, static. Replace
`animate-pulse` at every site where the label text itself pulses — the `Working` line, the `Queued`
line, and the held-forward `↪ waiting for…` line, which has the same pulsing-text problem.

The label names the number, so exactly one number is on screen and its meaning is never ambiguous:

| State | Renders |
| --- | --- |
| Queued | `Queued` + dots. No number — a queued send has no `started_at` (it is not a turn yet). |
| Running | `Working` + dots + elapsed since `turn.started_at`. |
| Past the no-response threshold | `No response` + dots + the existing silence counter. |
| Complete | `Worked for 2m 14s`. No dots. |

The only thing that changes at the heartbeat threshold is the word, and therefore the quantity the
word names. `Working 2m 14s` means "working for 2m 14s"; `No response 1m 02s` means "no response for
1m 02s". Both counters already have a home: elapsed is new, silence is `quietElapsedMs` and stays
exactly as it is.

Three facts to build against, all verified — do not re-derive them:

- **The 1 Hz ticker already exists** (`UnifiedTranscript.svelte:578`). Reuse `now`; do not add a
  per-turn `setInterval`. Its doc-comment currently asserts that `now` is read only inside the quiet
  footer, so ticks trigger no re-render when nothing is quiet. **The elapsed counter breaks that
  invariant** — running turns will now read `now` every second. The cost is one text node per running
  turn per second, which is fine, but the comment must be rewritten rather than left to rot.
- **`formatDuration` (`utils.ts:50`) renders `0m 09s`** for a nine-second turn. It is only used by the
  silence counter today, where sub-minute values are impossible by construction (the counter starts at
  one full `HEARTBEAT_TIMEOUT_MS` = 60 s). Elapsed turns are frequently under a minute, so it needs a
  sub-minute form (`9s`). Extend it or add a sibling; do not leave `0m 09s` on the common case.
- **An agent turn carries `started_at` and `ended_at`** (`state/types.ts:63-64`), so both the live
  counter and the completed total derive from existing state. Nothing new crosses the wire.

Scope note: `--status-processing` and `--warning` are currently the *same hex* in both themes
(`#b45309` / `#fbbf24`). Any future design that tries to distinguish a state by swapping between them
is a no-op. Not a problem here, since this design uses no state colors — recorded so the next person
does not rediscover it the hard way.

### Definition of Done

- Component test: a roster with two long shared-prefix names renders two distinguishable labels.
- Visual verification with agents in each status (idle, processing, failed, cancelled), light and dark.
- Component tests on the live-turn footer: a queued turn renders dots and **no** number; a running turn
  renders dots and an elapsed count; crossing the heartbeat threshold swaps the word to `No response`
  and the number to the silence counter, with the dots unchanged; a completed turn renders
  `Worked for …` and no dots.
- Unit tests on `formatDuration`'s sub-minute form: `9s`, `59s`, `1m 00s`, `1h 04m`. The existing
  silence-counter callers must keep their current output — that is the regression risk of touching a
  shared formatter.
- The elapsed counter must be deterministic in tests: inject the clock, never read wall time
  (`AGENTS.md` forbids time-of-day dependencies in unit tests).
- The ticking number must not be announced by assistive tech. Assert it is outside any `aria-live`
  region; state transitions are what get announced, not seconds.

### As-built decisions (recorded at implementation review)

- **No completed-turn duration** (engineer decision at review). The plan's `Worked for 2m 14s` state
  would have rendered under *every* settled turn in the transcript — hydrated history included —
  which is new information under every past turn, not a restyle. Dropped: completed turns render no
  live footer, no dots, nothing new. The elapsed counter exists only while a turn runs. The DoD's
  completed-turn test asserts absence instead.
- **No status dot** (engineer decision at visual review). A first pass added an idle/processing
  dot (the DoD's "failed, cancelled" states don't exist at the agent level — `run_status` is
  deliberately `idle | starting | processing`; failures render in the transcript). Rejected on
  sight: the card's name row has no room to spend, and idle-vs-processing wasn't worth the pixels.
  The "run status is the most salient thing on the card" outcome is dropped with it.
- **Name truncation fixed by reclaiming the icon gutter, not by wrapping.** A first-pass two-line
  wrap was rejected at visual review. The real thief was the hover-revealed action icons
  (eye + actions trigger): `opacity-0` kept them invisible but still reserved a two-icon gutter on
  every card. They are now `hidden` (zero width) until hover/focus/open, so the single-line name
  keeps the full row width at rest and truncates only while the icons are actually shown —
  shared-prefix names are distinguishable exactly when the user is reading, and the eye stays
  visible while an agent is hidden (it's the state indicator).
- **Only actionable card regions hover.** The raised card itself is static because clicking its
  body has no action. A taller name-row button carries the collapse hover and a chevron; the drag
  grip sits outside it so a plain grip click is inert. Double-clicking only the name enters rename.
  Eye, actions, and rename-save controls use the stronger on-raised icon-button treatment.
- **`formatDuration` gained the bare-seconds form in place** (`9s` under a minute); the silence
  counter can't hit that branch (starts at one heartbeat threshold = 60s), so its output is
  unchanged — asserted by the untouched ≥1m test cases.
- **Chips are hand-rolled quiet spans** (`bg-panel`, icon + count, tooltip with the full phrase),
  not `Badge` — Badge's uppercase-label semantics don't fit a count. Plug = MCP, Zap = skills.
- The `now` ticker's doc-comment was rewritten (running turns now read it every second); elapsed
  derives from the existing `started_at` — nothing new crosses the wire.
- **Held-forward row names only its sources** (`↪ waiting for bob`): the row renders in the
  recipient's own pane under the forwarded body, so naming the recipient was redundant — and the
  old cross-pane "Forward to unknown" resolution bug is structurally impossible now.

---

## M9 — Transcript message navigator

Depends on M3/M4 (tool facets give tool-only turns a sensible preview) — both shipped. Scoped
2026-07-12 with the engineer; decisions below are settled product calls, not open questions.

### Goal & Outcome

A long transcript is only navigable by scrolling: no way to see the conversation's shape at a glance
or jump to a specific message without hunting. The navigator is a header-invoked popover — a
table-of-contents with search — that lists **every message** and jumps the transcript to the one you
pick.

Form (decided): a new header icon between the `+` (add pane) control and the compact toggle opens a
non-modal popover anchored at the right edge of the header. Inside: a search input, a role filter,
and a scrollable flat list of all messages in transcript order. Hovering (or arrow-keying) an entry
shows a larger preview panel to the entry's left (the Spotlight pattern — list right, preview left,
which is where the screen space is). Clicking/↵ jumps the transcript so that message lands at the top
of the view. Zero persistent chrome; costs nothing until invoked.

Rejected alternatives from the original sketch, for the record: gutter markers (saturate on long
histories, fight the M4 left-rule idiom), a persistent outline rail (permanent horizontal cost for
occasional use), a minimap (most rendering work, least readable). A palette-source contribution
(reusing `setCommandSource`) can layer on later using the same index — complement, not the form.

When this milestone is done:

- Scan the whole conversation and read a preview of any message without scrolling the transcript.
- Type to filter messages by content; filter by role (All · You · Agents).
- Jump to any message — including one outside the render window, and one whose pane is minimized or
  maximized-over — landing with it at the top of the view.
- The list reflects live updates as turns arrive.
- A hairline vertical divider separates the header's app-level controls (Projects/Git switcher) from
  the project-transcript controls (+ pane, navigator, compact, sidebar toggle).

### Settled decisions

- **Entries: every message, flat and sequential** (engineer decision — sends are often near-identical
  re-used prompts; the send *and* its responses are both retrieval context). One entry per user send
  (the row model already collapses a fan-out's user copies) and one attributed entry per agent
  response. Not a per-send spine.
- **Jump/pane rules reuse `revealPane`** — the same primitive behind ⌘⌥N and `@panename` targeting,
  so navigator jumps behave like every other pane-targeting gesture: a minimized target pane is
  restored; while another pane is maximized the target *replaces* it (focus stays focus). Agent
  message → its (unique) pane. User message → the leftmost pane containing any recipient. One pane
  moves per click; others untouched. An agent in **no** pane renders nowhere, so its entries show
  disabled with a tooltip naming the agent.
- **Search**: case-insensitive substring over whitespace-collapsed text, no fuzzy matching — the
  projects-sidebar search precedent. Searches the message's full prose (user text; an agent turn's
  text items concatenated), not just the preview line. Tool calls and thinking excluded in v1
  (they'd drown prose hits; a later toggle if missed). Preview line: first non-empty line, markdown
  syntax stripped (the M4 thinking-preview rule).
- **Role filter**: segmented `All · You · Agents` (existing `SegmentedSelect`), filtering both the
  list and the search domain.
- **Hover preview panel**: ~150ms debounce (no flashing while running down the list); full content
  rendered as markdown in its own scrollable region. ↑/↓ moves a highlight with the preview
  following; ↵ jumps; the whole popover is keyboard-operable.
- **Compact mode composes without a rule**: jumping never auto-expands anything — the row you land
  on is visible regardless of per-unit compact state.

### Constraints (verified against the code)

- **The transcript is render-windowed** (`UnifiedTranscript.svelte` — top-cursor tail window; the
  cursor only grows upward and freezes per conversation identity). Off-window messages are in memory
  but **not in the DOM**. The navigator must derive entries from the row model
  (`buildUnifiedRows` / `UnifiedRow`, `src/lib/state/unified.ts`), never the DOM; a jump must lower
  the cursor to mount the target (paying its markdown parse), wait for mount, then scroll — while
  suppressing the scroll-anchoring machinery whose whole job is preventing exactly that viewport
  motion. This is the risk, which is why the jump primitive is built and browser-tested first.
- **No backend, no wire changes** — everything derives from state already in memory.

### Implementation Outline

- **M9.1 — jump primitive (riskiest first).** `jumpToRow(key)` on `UnifiedTranscript`: re-pin the
  window cursor to include the target block, mount, scroll it to the top with anchor suppression.
  Pane layer integrates `revealPane` per the rules above. Browser tests: an off-window target on a
  long seeded transcript lands at top; a jump into a minimized pane restores it; a jump while
  another pane is maximized swaps the maximized pane.
- **M9.2 — message index.** Pure derivation `UnifiedRow[] → entries` (key, role, attribution,
  timestamp, preview line, searchable prose) plus the search/role-filter functions. Unit tests:
  fan-out dedup, attachment-only sends (filename as preview), forwards, tool-only turns (facet-based
  preview), markdown stripping, whitespace-collapsed matching.
- **M9.3 — the popover.** Header button + divider, search input, role segmented control, scrollable
  list, hover/keyboard preview panel, click/↵ → jump, live-append behavior, Esc/outside-click close.
  Component tests for filtering, keyboard nav, disabled unassigned-agent entries; visual pass is the
  engineer's.

### Definition of Done

- Browser test: jumping to a message far outside the render window lands it at the top of the view
  (measured geometry, `expect.poll`); the render window grew rather than re-pinned to tail.
- Browser or component tests for the two pane-reveal rules (minimized restore; maximized replace).
- Unit tests on the index derivation and matching rules listed in M9.2.
- Component tests: type-to-filter narrows the list; role filter restricts list and search; ↑/↓/↵
  navigate and jump; a disabled entry (agent in no pane) does not jump and names the agent.
- The preview panel renders the complete selected message in its own scrollable region.
- Visual verification (light + dark): popover anchoring at the right header edge, preview panel to
  the left, divider placement, long-list scrolling.

### As-built decisions (recorded at implementation review)

- **Jumps address row keys, not block keys.** Block grouping is pane-dependent (a fan-out is one
  `f:` block in a pane showing several recipients but plain rows in a single-recipient pane); row
  keys are stable across both shapes, so the executing transcript resolves the containing block
  itself. An agent response inside a fan-out lands at its block's top — the send with the response
  column right under it. This is a **separate v1 contract**, not implied by the list-granularity
  choice: picking send + response as the navigator's retrieval unit doesn't by itself dictate the
  scroll target, so landing a fan-out response on its send block is a deliberate v1 decision
  (pinned by a browser test). Per-response scroll precision (a `subRowKey` + per-column anchors) is
  deferred until the visual pass shows it's actually needed — no speculative plumbing.
- **The jump is a consumed, pane-addressed store request** (`state/transcriptJump.svelte.ts`),
  executed inside `UnifiedTranscript` because all three phases touch private state: window-cursor
  re-pin → mount → scroll → adopt the position as the anchor reference exactly the way `reanchor`
  ends a pass, so the anchoring machinery defends the jumped-to position. Consumption exists for
  the reveal path: a minimized pane's freshly-mounted transcript picks the pending request up on
  mount, and a consumed request can't replay on later remounts.
- **Fixed a latent pane-identity bug found by the jump's browser test**: an untouched project's
  default layout was rebuilt with a *random* pane UUID on every `layoutFor` read, so two readers
  disagreed about the same pane's id. The default pane id is now a stable sentinel
  (`"pane-default"`); uniqueness only matters within one project's layout.
- **Accepted cost, recorded**: jumping far back mounts everything from the target to the tail (the
  window is a tail window), paying the deferred markdown parse once — the user explicitly asked to
  go there, and the cursor only grows, so it's one-time per conversation.
- **Index entries carry no tool/thinking prose** (search is prose-only per the scoped rules), but
  tool-only turns preview via the tool-row vocabulary (`Command · cargo test`) and thinking-only
  turns via their first cleaned line. The index derives only while the popover is open, so a
  closed navigator costs nothing per streamed chunk.
- **Eye-hidden agents disable their entries** (tooltip names the agent) — same treatment as
  unassigned: their rows render in no pane, so a jump would land on nothing.
- The header divider is a `border-l` line, not a `bg-border` fill — the M2 token scan enforces
  border-as-line, and caught the first attempt.

Post-visual-review rework (the popover became a centered overlay):

- **Form changed from a header popover to a centered, focus-trapped overlay** (engineer decision at
  visual review — the 24rem popover squished the search next to the filter and gave no room to see
  the list was scrollable). It now reuses the command-palette idiom: a `Dialog` over a dimmed,
  `backdrop-blur-sm` transcript, list on the left, larger preview on the right. Dialog gained an
  `overlayClass` prop for the blur. Search gets its own full-width row; the role filter, a new
  **sort toggle**, and a message count share a second row.
- **Newest-first by default** with a sort-direction toggle (the index is chronological; the
  component reverses for descending). The common use is "jump to something recent," so the newest
  message is at the top without a scroll — accepting that the list order is then opposite the
  transcript's.
- **Opened three ways via a shared `navigatorState` store**: the header button (moved to the right
  of the compact toggle, left of the divider), **⌘F** (handled before the editable-target guard so
  it works from the compose box — there's no native find to preserve; gated to a project
  transcript), and a **"Find message…" command-palette entry**. The open navigator suppresses other
  window chords while it owns the keyboard, mirroring the palette.
- **Scroll affordance**: both the list and the preview fade at whichever edge has more content
  (`scrollFade` action toggles `data-fade-*`, CSS masks the faded edge). Preview prose dropped to
  `text-sm` with tightened block spacing.
- **Disabled (unassigned/hidden-agent) entries use the app `Tooltip`**, not the native `title`.
- **The centered dialog previews the complete message.** The original 1,500-character cap protected
  the smaller hover popover from repeatedly mounting large Markdown bodies. The dialog now gives
  the preview most of the window and its own vertical scroll region, so truncating available content
  conflicts with the dialog's purpose. The hover debounce still avoids rendering messages while the
  pointer merely passes over rows.

---

## Cross-milestone summary

| Milestone | Depends on | Backend? | Rough shape |
| --- | --- | --- | --- |
| M1 compose correctness | — | yes (GC param) | bug fixes |
| M2 token foundation | — | no | foundation |
| M3 tool facets | — | yes | foundation |
| M4 tool-call rows | M2, M3 | no | the headline change |
| M5 changed-files card | M4 | no | **skipped** — superseded by M4's inline diffs |
| M6 resize + layout | — | no | primitive + persistence |
| M7 Git view | M2 | no | polish |
| M8 sidebar + live-turn indicators | M2 | no | polish |
| M9 transcript message navigator | M3/M4, M6 | no | scoped — popover ToC + search + jump |

M1, M2, M3, and M6 have no dependencies on each other and could proceed in parallel if that is
useful. M4 is the milestone the whole plan is pointed at. M9 was scoped 2026-07-12 (it entered the
plan as a captured idea) and is the last remaining milestone.

Run `make check` before opening the PR; it includes the browser suite, which `make test` does not.
Adapter-touching work (M3) additionally requires `make test-live-claude` and `make test-live-codex`.
