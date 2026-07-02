# Attachments (drag-and-drop files into compose)

**Date:** 2026-06-07
**Branch:** `attachments`
**Status:** Plan — not yet implemented

## Problem

A user needs to attach files (images first, but any file) to a message and have
the receiving agent(s) act on them. The desired UX:

- Drag a file onto the compose box → it appears as a removable chip.
- An **Attachments** section in the `@` context menu lets the user insert an
  inline reference token (`` `image-1` ``) into the message, the same way file
  mentions already work, so they can write prose like "compare `` `image-1` ``
  and `` `image-2` ``".
- On send, the agent receives the file(s) so it can read them.
- Fan-out (one send → N agents) must work.

## How files actually reach the agent (the load-bearing constraint)

Every harness adapter (`crates/harness/src/{claude_code,codex,gemini,antigravity}/mod.rs`)
passes the user's prompt to the CLI as a **plain string argument** (positional
after `--`, or `--prompt=`), with `stdin` set to `null`. There is **no native
multimodal / image channel** into any of these CLIs through our adapters, and
there is no need to build one: all four harnesses have file-reading tools. So
the transport is: **stage the file on disk, then append its path to the prompt
text** and let the agent read it. This is harness-agnostic by construction and
is the correct approach for this architecture, not a workaround. Record this
rationale in the code (footer-renderer docstring) so a future reader doesn't
"upgrade" it to a per-harness image API.

**The file must be readable by the agent.** Agents run with `cwd =` the user's
bound working directory. Harness sandboxes/permission models differ (Claude:
`--dangerously-skip-permissions`; Antigravity: `--add-dir <cwd>`; Codex: sandbox
modes scoped to the workspace). The one location guaranteed readable by all four
is **inside `cwd`**. Switchboard's per-project state already lives at
`<cwd>/.switchboard/projects/<id>/` (the journal lives there). Staged
attachments therefore go in a sibling `attachments/` subdir of that per-project
dir, and the path appended to the prompt is **absolute**. This single decision
removes the entire "agent can't find the file" failure class; do **not** stage
into the OS temp dir.

## Lifecycle decision: persist + reference-GC (NOT delete-after-fan-out)

**Decision (settled): a sent attachment is durable conversation content and is
kept; we never delete on turn completion.** The initial sketch was "delete the
temp file after the last response"; it was rejected. Capture this rationale in
the GC code so it survives — three concrete reasons:

1. **Deleting reclaims almost nothing.** When the agent reads the file, its
   contents are captured into the harness's own session transcript
   (`~/.claude/projects/.../*.jsonl`, `~/.codex/...`) as a tool result and stay
   there permanently, outside Switchboard's control. So deleting our staged copy
   does not remove the bytes from disk in the common case — it only drops the one
   copy we control.
2. **Deleting breaks two consumers.** (a) The transcript bubble renders an image
   thumbnail from the staged path (M3) — deletion breaks every historical
   thumbnail on reopen. (b) A staged path is baked into the turn's prompt and
   lives in the harness session file forever, so a follow-up/resumed turn that
   re-reads the attachment needs the file to still resolve.
3. **Deleting is strictly more code, and more fragile.** The dispatcher has **no
   aggregate "all turns of this send finished" signal** — it runs one independent
   actor per agent; a send fans out to N independent turns correlated only by
   `send_id` on per-agent events. Delete-after-completion would require new
   send-scoped refcounting that decrements on *every* terminal (completed **and**
   failed **and** cancelled) and the never-started case, **plus** a GC fallback
   anyway for app-quit-mid-turn — i.e. it is "persist + GC" with extra fragile
   machinery layered on top.

So: reclaim disk with a **garbage-collection pass on project load** that deletes
any file in the attachments dir **not referenced by any `journal.jsonl` `Send`
record**. This is a pure function of on-disk state: crash-safe (just re-runs next
load), needs no completion signal, cleans up files that were staged-on-drop but
never sent (orphans), and preserves every sent attachment as long as its
conversation exists. In a fan-out, all N `Send` records reference the **same**
single staged file — no per-recipient copies, and GC keeps it while any
recipient's `Send` references it.

If unbounded growth ever becomes a real problem, the clean follow-up is a
size-/age-cap eviction folded into the **same** GC pass — a small additive knob,
not a redesign. Explicitly deferred (see Non-goals); do not build it now.

## What the user sees vs. what the agent receives

Keep these separate (this is why attachments are structured, not just text
smuggled into the prompt):

- **To the agent:** clean prompt text **+** an appended footer listing
  `label: <absolute path>` per attachment. The footer exists **only** in the
  string handed to the adapter.
- **In the journal and the UI:** the **clean** prompt text **plus** a structured
  `attachments` array. The user-message bubble renders the clean text and
  attachment chips; it never shows the footer or raw paths.

The user message is rendered from the journal `Send` record (not the harness
file — see `src/lib/types.ts:531-551` and system-design §3), so storing clean
text + structured attachments on `Send` is sufficient to get clean display while
the footer reaches the agent.

## Cross-cutting contracts (establish in M1, reuse everywhere)

These are defined once in `crates/core` (M1) and reused by the dispatcher, app,
and frontend. Later milestones must not invent parallel shapes.

1. **`Attachment` type** (new, in `crates/core`, serde, snake_case wire,
   `#[non_exhaustive]`): the metadata for one staged file. Fields:
   - `label: String` — the user-facing reference, e.g. `image-1` (assigned by
     the **frontend**; see below).
   - `kind` — enum `image | text | file` (the classification that drives the
     label prefix), plus an `unknown` deserialize-only fallback so a kind written
     by a newer build never fails an older build's journal load (a display-only
     hint must not brick history). The frontend only ever emits the three real
     kinds; `unknown` appears only on cross-version reads and renders as a generic
     file.
   - `path: String` — absolute path to the staged file under the project's
     `attachments/` dir.
   - `original_name: String` — the dropped file's basename, for display.
2. **Kind classification by extension** — a documented, single-source mapping:
   common image extensions (`.png .jpg .jpeg .gif .webp .svg .bmp .heic` …) →
   `image`; common text/code extensions (`.txt .md .json .csv .log .rs .ts .py`
   …) → `text`; everything else → `file`. The **frontend** owns this (it has the
   dropped filename and assigns the numbered label); the backend only persists
   what it's told. Document the extension lists where they live so they're easy
   to extend.
3. **Label assignment** — frontend, per compose session: per-kind counters
   (`image-1`, `image-2`, `text-1`, `file-1`, …). Labels are **stable**: removing
   a chip does **not** renumber the others. The inline `@`-menu token and the
   footer use the same label.
4. **Footer format** — a pure function `render_prompt_with_attachments(prompt,
   &[Attachment]) -> String` in `crates/core`. When the list is non-empty, append
   a separator and one `label: <absolute path>` line per attachment, preceded by
   a short explicit lead-in so the agent understands these are files to read,
   e.g.:

   ```
   <clean prompt>

   ---
   Attached files (read them):
   image-1: /Users/.../.switchboard/projects/<id>/attachments/<uuid>__diagram.png
   text-1: /Users/.../.switchboard/projects/<id>/attachments/<uuid>__notes.txt
   ```

   Empty list → return the prompt unchanged. The exact lead-in wording is the
   implementer's to finalize; the pinned contract is "separator + lead-in + one
   `label: absolute_path` line per attachment, appended only when non-empty."
5. **Footer is built at dispatch time, in the dispatcher** — the adapters stay
   unchanged and attachment-unaware (preserving harness-agnosticism). The
   dispatcher stores the **clean** prompt + attachments on the `WorkItem`,
   journals the clean prompt + attachments, and calls
   `render_prompt_with_attachments` immediately before `adapter.dispatch(...)`.

## Docs the implementing agent MUST read before starting

- `docs/system-design.md` §3 (filesystem layout + split source-of-truth) and §7
  (sends/turns) — the attachments dir and `Send` shape change touch both.
- `docs/harness-behavior.md` — record the per-harness file-read behavior
  and the new live tests here.
- `docs/ui-conventions.md` — chips/drag-over states must use existing `ui/`
  primitives and semantic tokens.
- Tauri v2 drag-and-drop (verify against installed version):
  - Webview drag-drop event: `onDragDropEvent` in `@tauri-apps/api/webview` —
    https://v2.tauri.app/reference/javascript/api/namespacewebview/
  - Window `dragDropEnabled` config (must stay enabled for the event to fire;
    note OS file drops do **not** raise HTML5 `drop` events when this is on):
    https://v2.tauri.app/reference/config/
  - Drag-and-drop concepts/guide: https://v2.tauri.app/learn/window-customization/
    (and search the docs for "drag drop" if the path has moved).

---

## Milestone 1 — Core data model + footer rendering

### Goal & Outcome

Establish the shared `Attachment` contract, persist it on the journal `Send`
record, and provide the pure footer renderer — the pieces every later milestone
reuses. No behavior change visible to the user yet.

Outcomes:

- An `Attachment` value type exists in `crates/core` and round-trips through
  JSONL.
- `JournalRecord::Send` carries an `attachments` list; old journals (records
  without the field) still deserialize (empty list).
- A pure `render_prompt_with_attachments` function produces the agent-facing
  footer and is unit-tested against the format contract.

### Implementation Outline

- Add the `Attachment` struct and `kind` enum to `crates/core` (serde,
  `rename_all = "snake_case"`, `#[non_exhaustive]` on the enum per the IPC-evolution
  convention). This is the type the dispatcher, app hydration, and the TS wire
  type all mirror.
- Add `attachments: Vec<Attachment>` to `JournalRecord::Send`. **Backwards
  compatibility is required**: existing `journal.jsonl` files have `Send` lines
  with no `attachments` key, and the journal reader is fail-loud on corrupt
  lines — so the field must default to an empty vec on deserialize
  (`#[serde(default)]`). Add a test that an old-shape `Send` line (no
  `attachments`) still parses.
- Add `render_prompt_with_attachments(prompt: &str, attachments: &[Attachment])
  -> String` in `crates/core`. Put the rationale from "How files reach the agent"
  into its docstring (why path-in-text, not a per-harness image API). Empty list
  → prompt returned unchanged (no trailing separator).

### Definition of Done

- Unit tests: `Attachment` + `Send`-with-attachments JSONL round-trip; old-shape
  `Send` (missing `attachments`) deserializes to an empty list; footer renderer
  with zero / one / many attachments and the exact `label: path` line shape;
  footer renderer leaves the prompt untouched when the list is empty.
- No doc changes required yet (data-model only); system-design update lands with
  M2 when the on-disk dir and send path are wired.

---

## Milestone 2 — Backend: staging, send wiring, hydration, GC

### Goal & Outcome

Wire attachments end-to-end through the backend: a command to stage a dropped
file, threading attachments through the send path so the agent receives the
footer and the journal stores the structured list, exposing attachments on the
hydrated user message, and reclaiming orphaned files on load.

Outcomes:

- A frontend can call a command to copy a dropped file into the project's
  attachments dir and get back the staged absolute path.
- Sending a message with attachments causes the target agent(s) to receive the
  prompt with the appended file paths; the agent can read the files.
- A fan-out send references one staged file from all N recipients' `Send`
  records (no duplicate copies).
- On project reopen, the user message shows its attachments; the agent responses
  are unaffected.
- Attachment files not referenced by any `Send` record are deleted on project
  load.

### Implementation Outline

- **Staging command** (`stage_attachment`, thin `#[tauri::command]` over
  `stage_attachment_impl` in `commands.rs`, mirrored in `src/lib/api.ts`):
  - Input: project id + the **source path** of the dropped file (Tauri's
    drag-drop event yields OS file paths for Finder drops — see M3). Returns the
    staged absolute path (and the original basename).
  - Copies source → `<project-meta-dir>/attachments/<uuid>__<sanitized-basename>`.
    Reuse the existing per-project-dir resolver (the one the journal uses);
    create `attachments/` if absent. The `uuid` prefix avoids collisions; sanitize
    the basename (no path separators / traversal). Copying happens in Rust, so no
    frontend fs-plugin permission is involved.
  - Classification/labeling is **not** done here (frontend owns it, per the M1
    contracts) — this command only places bytes and returns the path.
- **Thread attachments through the send path:**
  - `send_message_impl` and the `send_message` tauri command gain an
    `attachments: Vec<Attachment>` parameter (mirror in `api.ts sendMessage`).
    Keep `prompt` the **clean** text.
  - `Dispatcher::send_message` and the internal `WorkItem` gain `attachments`
    (clean prompt stays on `WorkItem.prompt`).
  - The `ConversationJournal::record_send` trait method gains `attachments` and
    writes them onto the `Send` record (clean prompt + structured attachments).
  - In the actor's turn path, build the agent-facing prompt with the M1
    `render_prompt_with_attachments(&item.prompt, &item.attachments)` **at the
    `adapter.dispatch(...)` call site only**. Adapters are unchanged.
  - **Edge case — queued sends:** attachments travel on the `WorkItem`, so the
    footer is rendered when the queued item finally dispatches (not at enqueue).
    `remove_queued_message_impl` returns a payload "so the compose bar can restore
    the text" — extend that payload to also carry the attachments so dequeue
    restores the chips, consistent with how it already restores the prompt.
- **Hydration:** the `user_message` `ConversationItem` (built in `commands.rs`
  from grouped `Send` records) gains the `attachments` list, read straight off
  the `Send`. Fan-out grouping is unchanged (group by `send_id`); the attachments
  come from the grouped `Send` (identical across recipients).
- **GC on load:** in the project-load/hydration path (where `journal.jsonl` is
  already read), after reading records, list the `attachments/` dir and delete
  any file whose absolute path is not referenced by any `Send.attachments`.
  Best-effort: a failed unlink logs a warning and does not fail the load
  (mirrors the registry "degrade with a warning" posture). This is the only
  place attachments are deleted.

### Definition of Done

- Unit/fixture tests:
  - `stage_attachment_impl` copies into the project attachments dir, returns an
    absolute path, handles a basename with awkward characters, and is collision-safe
    across two stages of the same filename.
  - Send with attachments: the journal `Send` stores clean prompt + attachments;
    the string passed to a `MockHarnessAdapter` is the footered prompt (assert the
    `label: path` lines are present and the journal copy is clean).
  - Fan-out: two recipients, one staged file, two `Send` records referencing the
    same path; hydrated user message exposes the attachments once.
  - GC: a file referenced by a `Send` survives; an unreferenced file (orphaned
    drop) is removed; a non-existent dir is a no-op; an unlink failure degrades to
    a warning, not a load failure.
  - Dequeue round-trips attachments in the restored payload.
- **Live tests** (adapter-touching → required per AGENTS.md; name
  `live_<harness>_attachment_...`): for each harness, stage a tiny text file with
  known contents under the project dir, send a prompt instructing the agent to
  echo one word from the file, assert the response contains it. This proves the
  cwd-staging location is actually readable under each harness's sandbox — the
  central risk of this feature. If a harness cannot read it, record the gap in
  `docs/harness-behavior.md` and add a user-facing line to the README's
  "Harness support and limitations".
- Docs: update `system-design.md` §3 filesystem layout (new `attachments/` dir
  under the per-project metadata dir; `Send` record gains `attachments`) and the
  split-source-of-truth note (attachments are user-side, journal-owned). Note in
  the layout that the dir is runtime data users gitignore (consistent with the
  rest of `.switchboard/` runtime state).

---

## Milestone 3 — Frontend: drag-drop, chips, `@`-menu section, rendering

### Goal & Outcome

The full compose UX, built on the M1/M2 backend.

Outcomes:

- Dragging file(s) onto the compose box stages them and shows a removable chip
  per file (`image-1`, `text-1`, `file-1`, …), with a drag-over highlight.
- The `@` menu has an **Attachments** section listing the current chips;
  selecting one inserts its inline reference token (`` `image-1` ``) into the
  draft, exactly like a file mention.
- Sending includes the attachments; the compose clears them on a successful send.
- The user-message bubble renders attachment chips (image thumbnail where it's an
  image, filename otherwise) — never the raw footer/paths.
- Chips are session-scoped: they work for the lifetime of the open compose bar but
  are **not** restored across a project close/reopen (the load-time GC reclaims the
  unsent staged file). See Non-goals. Sent attachments persist as durable
  conversation content and always re-render on reopen.

### Implementation Outline

- **Drag-and-drop** via Tauri's webview `onDragDropEvent` (HTML5 `drop` does not
  fire for OS file drops while `dragDropEnabled` is on — read the docs above).
  The event carries file paths and a window position. On `drop`, check the
  position against the compose area's bounding rect; if inside, call
  `stage_attachment` for each path and add a chip. Use the `enter`/`over`/`leave`
  phases to drive a drag-over highlight on the compose box. Handle multiple files
  in one drop.
- **Attachment chips:** a new `$state` list on `ComposeBar` holding
  `{ id, label, kind, path, original_name }`. The frontend assigns `label` from
  per-kind counters using the M1 extension→kind mapping (defined frontend-side,
  in one module). Removing a chip (`×`) drops it from the list and does **not**
  renumber survivors. Chips drive the footer (the whole set is sent), independent
  of which inline tokens appear in the text.
- **`@`-menu Attachments section:** add an `AttachmentMenuItem` variant to the
  existing `MenuItem` union and render it as a third section in the mention menu.
  Selecting one inserts `markdownCodeSpan(label)` via the existing
  `insertFileMention` mechanism (`ComposeBar.svelte:445`) — reuse it; do not
  fork a parallel insertion path. The section lists current chips (optionally
  filtered by the `@`-token query).
- **Send wiring:** `dispatchToRecipients` passes the chip list (mapped to the
  `Attachment` wire shape) to `api.sendMessage`. The **clean** draft text is sent
  as `prompt` (the backend renders the footer). Clear chips on successful send,
  alongside the existing draft clear. Chips live **only** in `ComposeBar` `$state`,
  not the `ComposeSnapshot` — do **not** persist them. The draft *text* persists as
  today, but staged-but-unsent chips are deliberately session-scoped: the load-time
  GC deletes any unsent staged file, so a restored chip would dangle at a path that
  no longer exists (see Non-goals). `parseSnapshot` therefore needs no
  attachment-parsing change.
- **Transcript rendering:** the `user_message` `ConversationItem` (TS, mirroring
  the Rust change) gains `attachments`. Render them as chips under the message
  text; for `kind: image`, show a thumbnail from the staged path; any unrecognized
  kind (the `unknown` cross-version fallback) renders as a generic file chip, not
  a thumbnail. Reuse `ui/` primitives and semantic tokens per `ui-conventions.md`.
- **Wire type:** add the `attachments` field to the `user_message` arm in
  `src/lib/types.ts` and the `sendMessage` payload; degrade gracefully if absent
  (default `[]`), per the additive-variant convention.

### Definition of Done

- Component tests (mock `invoke`/`listen`, per AGENTS.md's component-test rule):
  - A simulated drag-drop stages files and renders the right labeled chips
    (`image-1`/`text-1`/`file-1` by extension; fallback to `file-`).
  - Removing a chip does not renumber the others.
  - Selecting an Attachments menu item inserts the `` `label` `` token at the
    cursor.
  - Sending passes clean text + the attachment list to `sendMessage` and clears
    chips on success; on send error chips are retained (mirror existing
    send-error handling).
  - A `user_message` with attachments renders chips/thumbnail and never shows a
    raw path.
  - After a reload, the compose bar restores the draft text but **no** chips
    (chips are session-scoped; the unsent staged files were GC'd on load).
- Manual verification: drag a real image from Finder onto compose, send to two
  agents, confirm both read it and the bubble shows a thumbnail.
- Docs: README "Harness support and limitations" entry only if a harness was
  found (M2 live tests) unable to read attachments; `ui-conventions.md` only if a
  new shared primitive was introduced.

---

## Non-goals / known limitations (explicitly out of scope)

- **Paste-from-clipboard images** and **browser-internal drags** (byte payloads
  rather than OS file paths) — only OS file drops (which yield paths) are in
  scope. Note in code where the drop handler assumes paths.
- **Staged-but-unsent chips do not survive a project close/reopen.** Staging
  copies the file to disk on drop, but the journal `Send` record (the only GC
  anchor) is written at send time — so an unsent staged file is an orphan the
  load-time GC reclaims. Persisting the chip across reload would dangle it at a
  deleted path, so chips are deliberately session-scoped: re-drop to re-attach.
  Sent attachments are durable. (The cleaner alternative — stage at send time so
  nothing is on disk to GC until a `Send` references it — was considered and
  deferred; revisit if the re-drop friction proves annoying.)
- **No file-size cap** on staged attachments. Not discussed; flag as a known
  limitation rather than inventing a policy. Add later if it bites.
- **No size-based or age-based eviction** beyond reference-GC. Disk grows with
  sent attachments by design (history fidelity); revisit only if it becomes a
  real problem.
- **No native multimodal/image API** per harness — path-in-text is the
  deliberate transport (see rationale above).
