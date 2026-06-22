# Transcript render windowing — fast load for long conversations

**Status:** Implemented (M1 + M2). See **As-built** below for what shipped and where it deviated from this plan.
**Date:** 2026-06-21
**Area:** Frontend (`src/lib/components/UnifiedTranscript.svelte`)

## As-built (outcome)

The forward-looking milestone text below is preserved as written; this section records what actually shipped, including one material deviation. Where the two disagree, this section is authoritative.

- **M1 — windowed render.** Renders `blocks.slice(firstVisibleIndex)`; a top-cursor index, never a sliding tail. The cursor is **seeded on `loadStatus === "complete"`** (the content-settle signal — the component mounts before history loads), and reseeds whenever the **conversation identity** changes, where identity = `projectId` + the **visible-agent list (in order)** + the **oldest block's key**. The visible-agent term is load-bearing: a pane hides/shows an agent on the *same* persistent component, and without it a stale cursor sliced a shorter `blocks` to empty (blank pane). The oldest-key term catches a late retry-hydration inserting history at the front. The window is also bounded **while loading** (so stale mid-load content can't mount unbounded), but the seed is only recorded on `complete`.

- **M2 — upward reveal.** A sentinel above the block list (kept OUTSIDE the `captureAnchor`-scanned `content`) is watched by an IntersectionObserver; reaching it mounts the next older batch. Reading-position stability is **not** hand-rolled — the prepend grows `content`, the existing `ResizeObserver → reanchor` holds the anchor. A one-batch latch gates re-entry and **coalesces** (does not drop) a trigger that arrives mid-reveal.

- **Deviation — Option A: `content-visibility` containment was REMOVED** (this plan had listed keeping it, and removing it, as out of scope). Reason: containment gives off-screen blocks *estimated* heights, and those estimates flip to real mid-correction, shifting the reading position ~a block — it was the confounder breaking the M2 reveal. With real heights the existing `reanchor` holds a top-prepend exactly, so windowing owns mounted-set size and scroll-anchoring owns position stability, with no estimate machinery between them. Windowing already bounds the mounted set, so containment's perf role is largely subsumed. **Residual:** revealing deep history grows the mounted set, so a forced relayout scales with it (measured in WebKit: ~3 ms at the default ~50 blocks, ~18 ms once ~300 are revealed). Past that the answer is true sliding-window virtualization — a deferred ceiling alongside backend cursoring, not CSS containment estimates.

## Problem

Opening a project with a long-running agent is slow. The motivating case is the
`switchboard-workflows` project's `coder` agent: a 38 MB / 13,905-record Claude
session file that the loader reconstructs into **431 turns (211 user + 220
agent) holding 4,230 render items** and serializes to **~8.76 MB of JSON** over
IPC — and that is one of seven agents merged on project open.

### Where the time actually goes (measured, not assumed)

Benchmarking the real file through the actual parser
(`load_claude_transcript`) settles where the cost is:

| Stage | Release (shipped app) | Debug (`make dev`) |
| --- | --- | --- |
| Rust parse of the 38 MB file | ~90 ms | ~610 ms |
| Rust serialize of the turns | ~10 ms | ~235 ms |

The backend parse is **~100 ms in a release build**. It is not the bottleneck.
The cost is the **frontend render**: `UnifiedTranscript.svelte` mounts *every*
block into the DOM (no virtualization — see the deliberate "Containment chosen
over virtualization" comment), and every text segment runs
`renderMarkdown()` — marked parse + Prism highlight + DOMPurify sanitize —
**synchronously in a `$derived` on mount**. With ~4,000+ items that is thousands
of synchronous parse/highlight passes plus a multi-thousand-node DOM build, all
on first paint.

The existing `content-visibility: auto` containment does **not** fix this: it
skips *layout and paint* for off-screen blocks, but the blocks are still
mounted, their Markdown components still instantiated, and `renderMarkdown` still
runs. Containment was built for the compose-textarea per-keystroke reflow cost,
not for initial mount.

### Decision: window the render, leave the backend alone

The fix is to **render only a window of blocks** — the last N — and mount older
blocks progressively as the user scrolls up, which is exactly the "load last N,
lazy-load more on scroll up" behavior requested. Off-screen blocks that are never
mounted never parse their Markdown.

We deliberately do **not** add backend cursoring / tail-parsing (see
[Out of scope](#out-of-scope)). At ~100 ms release parse it buys little, and
Claude's JSONL can't be cleanly parsed from the tail (tool results reference
earlier tool calls; turn grouping and the provenance/merge logic all depend on
whole-file ordering). The backend keeps returning the full merged conversation;
only the render is windowed.

### Pre-flight check (do this first)

Confirm the slowness reproduces in a **release build**, not only under
`make dev`. The shipped app is ~6× faster on the Rust side. If the lag is debug-
only, re-scope with the author before building — the render cost is still real
on huge transcripts, but the urgency and target numbers change.

**Also decompose the JS side before building — windowing only reduces one of
four stages.** On project open the frontend runs: (1) IPC + `JSON.parse` of the
~8.76 MB payload, (2) `buildUnifiedRows` over all turns, (3) `groupRenderBlocks`
over all rows, (4) mount + eager `renderMarkdown` of the blocks. The window
slices `blocks` _after_ stage 3, so **windowing only shrinks stage 4 (and only
its mount half)**; stages 1–3 process the full 431-block / 4,230-item set
regardless. The Rust benchmark proves the backend isn't the bottleneck but says
nothing about which JS stage dominates. Before committing to M1, wrap
`console.time`/`console.timeEnd` around those four stages on the real `coder`
payload in a release build. This is a quick four-probe measurement, **not** a
research project — if stage 4 dominates (the working hypothesis: thousands of
synchronous marked + Prism + DOMPurify passes plus a multi-thousand-node DOM
build outweigh a one-shot deserialize and two O(n) transforms), proceed. If
stages 1–3 turn out material, they need their own treatment and windowing won't
substitute — surface that to the author rather than expanding scope unasked.

## Required reading before implementing

The implementing agent must read these before touching code:

- `src/lib/components/UnifiedTranscript.svelte` in full — especially the
  scroll-anchoring machinery (`pinned`, `reanchor`, `captureAnchor`, the
  `kids`-based anchor search), the containment block (the `CONTAINMENT_*`
  constants and the comment at the "Containment chosen over virtualization"
  marker), and the `{#each blocks …}` render site.
- `src/lib/state/` block/row builders: `buildUnifiedRows` and `groupRenderBlocks`
  (the `RenderBlock` / fan-out column model the window slices over).
- `tests/browser/` mount + seed helpers and an existing `*.browser.test.ts` for
  the scroll-measurement testing pattern (poll geometry, never sleep).
- MDN — `content-visibility` & `contain-intrinsic-size`:
  https://developer.mozilla.org/en-US/docs/Web/CSS/content-visibility
- MDN — IntersectionObserver API:
  https://developer.mozilla.org/en-US/docs/Web/API/Intersection_Observer_API
- MDN — `overflow-anchor` (context for *why* anchoring is hand-built: WebKit
  doesn't implement it):
  https://developer.mozilla.org/en-US/docs/Web/CSS/overflow-anchor
- Svelte 5 keyed `{#each}` and runes (`$derived`, `$effect`):
  https://svelte.dev/docs/svelte/each

---

## Milestone 1 — Window the render to the last N blocks

### Goal & Outcome

Render only the tail of the transcript on load; the rest stays in memory but
unmounted. This is the milestone that delivers the load-time win.

Functional outcomes once complete:

- Opening a project with a long transcript paints quickly: only a bounded window
  of the most recent blocks is in the DOM, so only those blocks' Markdown is
  parsed on first paint.
- The transcript still opens pinned to the bottom showing the newest messages
  (unchanged behavior — the window *is* the bottom of the stream).
- New turns arriving (live dispatch, streaming, append) render and auto-scroll
  exactly as today; the window never drops the bottom of the conversation.
- A transcript shorter than the window renders in full with no behavior change
  whatsoever.
- No visible difference for the user yet *except* speed and that scrolling up
  past the window stops (M2 adds the upward reveal).

### Implementation Outline

The window unit is the **`RenderBlock`** (post-`groupRenderBlocks`), never the
raw turn — slicing turns would split a fan-out across the boundary. Add a derived
view that renders `blocks` from a top cursor to the end, and change the
`{#each blocks …}` site to iterate that derived slice (same keys, so Svelte keeps
mounted blocks mounted and only changes which are present).

Model the window as a **top-cursor index** (`firstVisibleIndex` into `blocks`),
**not** a fixed-size "last N" tail. This is load-bearing and chosen deliberately
over a tail count for two reasons spelled out in the existing containment comment
and the scroll machinery:

- A fixed-size tail **flips membership on every new turn** — as the conversation
  grows, an index-based last-N window would unmount the oldest visible block each
  time a new one appends, churning remembered `content-visibility` sizes and
  nudging the viewport. A top cursor pinned to the front of the window means new
  turns append *after* the cursor and are always rendered, while nothing already
  visible is unmounted.
- It makes M2's "reveal older" a single operation: decrement the cursor.

Seed value: `firstVisibleIndex = max(0, blocks.length - INITIAL_WINDOW)`. Pick
`INITIAL_WINDOW` as a small constant sized to comfortably exceed a viewport plus
scroll buffer (≈50 blocks is a reasonable starting point); it is a tuning knob,
not a contract — state in a comment that it's tunable against real feel and why
the default was chosen. Note in that comment that block count is a **loose proxy**
for the real cost driver (render *items* / `renderMarkdown` passes): one agent
block can hold dozens of items while a compact user row holds one, so a 50-block
window mounts a variable item count. Acceptable — keep the block-indexed cursor
(it's what keeps fan-out columns intact and keying stable); don't build
item-budget sizing unless the pre-flight shows a single pathological block is
itself the problem. The cursor only ever *decreases* (M2) or stays put; it must
never increase on append (which would unmount the bottom).

**Seed when the conversation's content settles — i.e. when `loadStatus` for this
identity reaches `complete` — not at mount and not on the first non-empty
`blocks` snapshot.** This is load-bearing and the single most consequential
detail in M1; getting it wrong ships a feature that passes its tests and does
nothing on the real open path. Why:

- `UnifiedTranscript` mounts *before* history loads. `TranscriptPanes` renders it
  gated only by pane membership (`visible.length`), **not** by `loadStatus`, and
  `hydrateProject` sets `{ items: [], status: "loading" }` first, fills content
  later. So at mount `blocks` is empty; seeding from `blocks.length` there yields
  `0`, and the "never increase on append" rule then renders the *entire* 431-block
  transcript when it arrives — the exact lag this milestone targets, untouched.
- Seeding on the *first non-empty* snapshot is also wrong, for a subtler reason:
  `hydrateProject` resets `conversations` (the `overlay`/`loadStatus`) at load
  start but does **not** clear the per-agent `transcripts` store that also feeds
  `blocks`. Re-opening a project whose agent streamed earlier this session leaves
  `blocks` non-empty with *stale* content before the fresh parse lands — first-
  non-empty would seed against that stale snapshot. `complete` is the unambiguous
  "fresh content for this identity has been written" signal. (Verified: on the
  initial open path `hydrateProject` writes all per-agent turns *and* flips
  `status: "complete"` in one synchronous burst after a single
  `await loadProjectConversation`, so `blocks` transitions empty → fully-populated
  in one reactive flush — `complete` and the content arrival coincide, so seeding
  on `complete` has no downside there while also covering the stale-reopen case.)

Apply the seed once per conversation identity, on its `loading → complete`
transition; do not re-apply it to ordinary live appends after that.

Cursor lifecycle / edge cases the agent must handle:

- **Reset + reseed on conversation identity change** (project switch, agent
  roster change): treat it as a fresh `loading → complete` cycle and seed to the
  new tail. Do not carry a stale index across a different `blocks` array. On a
  **refresh** (`hydrateProject`'s `isRefresh` path, which keeps the existing view
  and does *not* reset to `loading`), keep the current cursor — there's no
  identity change and no reseed.
- **`blocks.length <= INITIAL_WINDOW`** → cursor is 0; the `{#each}` renders
  everything; no window edge exists.
- **Front-stability assumption**: an index cursor is only safe because, *after*
  the settle, history grows at the *end* and streaming updates blocks *in place* —
  the front of `blocks` is stable. The one path that can insert historical turns
  mid-array post-settle is a late per-agent **retry hydration** (a failed agent's
  history loading after the project completed); treat that like an identity
  settle and reseed to the tail rather than letting the cursor point at a shifted
  index. Confirm the in-place-append shape against
  `groupRenderBlocks`/`buildUnifiedRows`; if any *routine* path inserts at the
  front, express the cursor against a stable key instead. State the assumption in
  a comment so a future change that violates it is caught.

> **Superseded by As-built (Option A).** This plan originally kept the
> `content-visibility` containment in M1. It was **removed** during M2: its
> off-screen height estimates broke the reveal's reading-position stability, and
> windowing already bounds the mounted set. See the As-built section.

Interaction with `pinned`/`reanchor`: because the window already *is* the bottom
and the component opens pinned, M1 should require no change to the pin-to-bottom
path. Verify that the first-paint pin still lands on the true bottom with the
window applied.

### Definition of Done

- **jsdom component tests** (extend `UnifiedTranscript.test.ts`):
  - **The headline guard — mount-then-hydrate.** Mount in `loadStatus: "loading"`
    with zero rows, *then* transition to `complete` with a >`INITIAL_WINDOW`
    transcript, and assert the rendered block count is bounded. This is the test
    that distinguishes a real fix from a test-only one: a naive "seed at mount"
    implementation passes a pre-seeded test but fails this one.
  - **The stale-reopen guard.** Mount with the per-agent `transcripts` store
    *already* holding content (simulating a re-open after earlier streaming) while
    `loadStatus` is `loading`, then settle to `complete` with the fresh
    >`INITIAL_WINDOW` set; assert still bounded — proves the seed waits for
    `complete` rather than firing on the stale first-non-empty snapshot.
  - Shorter-than-window conversation renders in full; appending a live turn after
    settle keeps every previously-visible block mounted and renders the new one
    (cursor does not advance); switching project/conversation reseeds to the new
    tail; the bottom (`last` block) is always present.
- **Browser test** (`tests/browser/`): on a long seeded transcript the initial
  DOM node count under the transcript is bounded (proves the window is real in
  WebKit, not just a jsdom artifact), and the view is pinned at the true bottom.
- Known limitation recorded in-code and in this plan: **select-all (⌘A) and any
  future find-in-page only cover mounted blocks** — windowing reintroduces the
  exact tradeoff the containment comment chose to avoid. Note it where the
  windowing logic lives. (Surfaced for sign-off in [Known limitations](#known-limitations).)
- The "Containment chosen over virtualization" comment is updated to reflect that
  windowing now bounds the mounted set, so it stays accurate for the next reader.

---

## Milestone 2 — Progressive upward reveal (scroll to load older)

### Goal & Outcome

Let the user scroll up past the initial window and have older blocks mount in
batches, without the viewport jumping — the "lazy-load another N as the user
scrolls up, with a spinner" half of the request.

Functional outcomes once complete:

- Scrolling toward the top of the window reveals the next batch of older blocks
  automatically; repeating walks all the way back to the start of the
  conversation.
- The reading position stays put when a batch is revealed — content the user is
  looking at does not jump as older blocks are prepended above it.
- A brief, honest affordance shows at the top while a batch is being revealed
  (and indicates more history exists above the window); it disappears once the
  cursor reaches the start of the conversation.
- A transcript shorter than the window shows no affordance and behaves exactly
  as today.

### Implementation Outline

Add a **top sentinel element**, rendered only while `firstVisibleIndex > 0`, and
observe it with an **IntersectionObserver** rooted on the scroll `container`;
when it approaches the viewport, **decrement `firstVisibleIndex` by a batch**
(`REVEAL_BATCH`, same tunable-constant treatment as `INITIAL_WINDOW`, clamped at
0). Use IntersectionObserver rather than reading `scrollTop` in `onScroll` so the
reveal is decoupled from the existing scroll/pin handler and can't perturb its
`pinned`/anchor bookkeeping. Disconnect/re-observe across conversation resets.

**Place the sentinel OUTSIDE the `content` element that `captureAnchor` scans**
(make it a sibling above `content`, not a child) — or, if it must live inside,
change `captureAnchor` to skip non-block chrome. `captureAnchor` binary-searches
`content.children` assuming every child is a transcript block in document order;
a sentinel/spinner child can be selected as `anchorEl` and then *vanish* when the
cursor reaches 0, corrupting the anchor exactly during a reveal. This is not
optional polish — it's a correctness guard for the anchoring M2 depends on.

**Scroll-position preservation on prepend: reuse the existing anchor machinery
first; do not hand-write a parallel correction by default.** Mounting older
blocks above the viewport grows `scrollHeight` at the top, which would otherwise
shove the user's content down. The component already solves this class of problem:
`reanchor` is driven by a `ResizeObserver` on `content` (so it fires on the
prepend's height change), and its anchor-restore branch holds the captured
block's viewport offset fixed whenever that block's *own* height is unchanged —
which is exactly the prepend case (older blocks mount *above* the read position;
the read block is untouched). Note also that a cursor decrement changes neither
`rows.length` nor the transcript revision, so the `scrollSignal` `$effect` does
**not** fire on reveal — the `ResizeObserver` is the sole, correct trigger.

So the **default M2 implementation is just**: sentinel + IntersectionObserver to
decrement the cursor (plus the sentinel-placement guard above and ensuring
`captureAnchor` reflects the pre-reveal anchor), and let `ResizeObserver →
reanchor` absorb the prepend. Do **not** add a manual "capture `scrollHeight`,
add the delta to `scrollTop`" correction unless the browser test below *proves*
`reanchor` can't hold the position — a second writer of `scrollTop` racing
`reanchor` in the same frame is precisely how double-jumps and anchor jitter get
introduced, and it would risk the pin-to-bottom / scroll-hold tests M2 promises
not to break. If the test forces a manual path, it must be the **single** writer
of `scrollTop` for the reveal — replacing the `reanchor` contribution for that
event, not stacked on top of it — sequenced after the batch has measurable height
(`tick()` / post-mount) and coordinated with the `lastScrollHeight` discrimination
so `onScroll` doesn't misread it as a user scroll.

Edge cases / constraints:

- Revealing must be a no-op for `pinned` state — the user is up in history, not
  at the bottom; a reveal must not pin-to-bottom them.
- Reaching the start (`firstVisibleIndex === 0`) removes the sentinel and stops
  the observer; no empty affordance lingers.
- A reveal that fires while another is still settling must not double-decrement
  past the user's intent or stack scroll corrections — guard against
  re-entrancy (e.g. ignore the observer while a reveal is in flight until layout
  settles).

The affordance: reveal is **synchronous** (data is already in memory) — there is
no fetch to await — so any spinner is only the brief mount/parse frame, not a
network wait. Keep it honest: a small "loading earlier messages" indicator at the
top (reuse the existing `Spinner` component) shown during the reveal frame, plus
the at-rest signal that older history exists above. Do **not** present it as a
network fetch or build a fake delay.

### Definition of Done

- **Browser test** (`tests/browser/`, the authoritative coverage for this
  milestone — the behavior is layout-coupled and jsdom can't measure scroll):
  on a long seeded transcript, scroll the container near the top, poll until the
  rendered block count grows, and assert the reading position is preserved — a
  reference **real transcript block** (never the sentinel/affordance) visible
  before the reveal stays at the same viewport offset (within tolerance) after it.
  Run this first **with no manual scroll correction in place** — it is the
  experiment that decides whether `reanchor` alone holds the position (expected)
  or a single-writer manual fallback is actually required. Cover repeated reveals
  walking back to block 0, after which the sentinel/affordance is gone. Use
  `expect.poll`/`expect.element` on measured geometry — never a fixed sleep.
- **jsdom component test**: the sentinel/affordance renders only while
  `firstVisibleIndex > 0` and disappears at 0; the reveal handler decrements by a
  batch and clamps at 0; a short transcript renders no sentinel. (Scroll-offset
  preservation itself is asserted in the browser test, not jsdom.)
- Existing scroll-anchoring browser tests (pin-to-bottom, footer-anchor,
  scroll-hold) still pass unchanged — windowing must not regress them.
- Known limitations resolved or recorded; the M1 select-all/find-in-page note
  remains accurate.

---

## Out of scope

- **Backend cursoring / tail-parsing.** The backend keeps returning the full
  merged `ProjectConversation`. Rationale above: ~100 ms release parse isn't the
  bottleneck, and partial parsing of Claude JSONL is unsafe (tool-result
  back-references, whole-file turn grouping, the provenance/merge ordering).
  If, after windowing, the ~8.76 MB IPC payload or JS-side memory becomes the
  next ceiling on very large projects, add backend cursoring as a *separate*
  follow-up — windowing is the prerequisite that makes it useful.
- ~~**Removing `content-visibility` containment.** Kept; complementary to
  windowing.~~ **Reversed in M2 (Option A) — containment was removed.** See
  As-built.
- **Deferred/idle Markdown rendering** (render plain text first, upgrade on
  view). A different optimization; windowing addresses the same cost more
  directly. Not built here.

## Known limitations

- **Select-all and find-in-page cover only mounted blocks.** Unmounted history
  (above the window, before the user has scrolled to it) is not in the DOM, so
  ⌘A / any future ⌘F won't include it until scrolled into the window. Inherent to
  windowing. **Accepted** (signed off before implementation): there is no current
  find-in-page feature, so the practical loss today is select-all over full
  history.
- **Deep-reveal layout cost.** Revealing a lot of history grows the mounted set,
  so a forced relayout (e.g. the compose bar's per-keystroke reflow) scales with
  it (~3 ms @ ~50 blocks, ~18 ms @ ~300, in WebKit). Acceptable now; the eventual
  answer is sliding-window virtualization, not the removed containment. See
  As-built.
