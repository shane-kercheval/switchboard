# Transcript performance improvements

**Status:** proposed.
**Branch:** `performance` (worktree `switchboard-performance`), based on `main` at `f2ab0aa` (includes PR #31).

## Problem statement

Typing in the compose bar is slow and laggy when the active project's transcript is large. The lag is present even when no agent is streaming; it gets worse while one is. Diagnosed root cause and contributing costs are below — read this section in full before implementing anything, because the fix only makes sense against the diagnosis.

### Root cause: per-keystroke forced reflow against an unbounded DOM

Two facts multiply into the symptom:

1. **The compose textarea forces synchronous full-document layout on every keystroke.** The compose bar uses `<Textarea autosize …>` (`src/lib/components/ComposeBar.svelte` ~line 1335). In `src/lib/components/ui/Textarea.svelte`, `resizeToContent()` writes `style.height = "auto"`, then reads `scrollHeight`, then calls `getComputedStyle(...).maxHeight`, then writes the final height. The write-then-read pattern forces the browser to synchronously recompute layout for the **entire document** before `scrollHeight` can return (classic layout thrashing). Worse, it runs **twice per keystroke**: once from the `oninput` handler and once from the `$effect` watching `value` — both call `resizeToContent`.

2. **The transcript DOM is unbounded.** `src/lib/components/UnifiedTranscript.svelte` renders every block of the conversation in a plain `{#each blocks}` (~line 1121) — no windowing. A long conversation means thousands of live nodes (markdown trees, Prism token spans, SVGs). The forced reflow in (1) is proportional to that whole layout tree.

Small transcript → cheap reflow → no lag. Large transcript → expensive reflow, twice per keystroke → lag. That is the observed size-dependence.

### What it is NOT

- **Not a Svelte re-render.** `draft` is local `$state` in `ComposeBar`; the transcript's reactive sources (`transcripts`, `runtimes`, `overlay`) don't depend on it. Svelte 5's fine-grained reactivity leaves the transcript untouched while typing. The cost is purely at the browser layout level.
- **Not streaming-only.** Streaming adds work (markdown re-parse of the growing segment, `scrollSignal` recompute, reducer array copies per chunk, re-anchor scrolls), so typing-while-streaming is the worst case — but the core lag exists with everything idle. Streaming-time costs were originally out of scope; after M1–M3 landed, hands-on testing confirmed idle typing is fixed while the streaming case persists, so it is now **M5** (added 2026-06-10). Its outline still obeys the original caveat: re-profile first, because PR #28 reworked that path.

### Secondary per-keystroke cost

The compose draft is persisted **synchronously on every keystroke**: a `$effect` in `ComposeBar.svelte` (~line 292) calls `setContent(...)`, and `composeStore.ts`'s `persist()` does `JSON.stringify` of the whole compose store + `localStorage.setItem`. This is O(draft + #projects), not O(transcript) — it does not explain the size-dependence, but it is real per-keystroke main-thread work. The module comment explains why writes are synchronous (a deferred write could race a send-clear or project switch); any fix must preserve those guarantees via explicit flush points.

### What PR #31 already changed (and didn't)

PR #31 (compact transcript mode + WebKit browser-test layer) landed after this diagnosis and matters here:

- **Compact mode is on by default** and genuinely removes DOM for collapsed units: tool-call and reasoning widgets are *not rendered* (`{#if}`-gated), and text bodies clip at 14rem. This shrinks the layout tree for typical long transcripts — a partial, conditional mitigation. But clipped text nodes **remain in the DOM** (`overflow:hidden` clips paint, not layout participation), the latest response and all streaming content stay fully expanded, and the user can expand anything — so DOM size is still O(history) in the general case. **Compact mode making typing feel better must not be read as "perf is fixed."**
- **The transcript now has a hand-built scroll-anchoring system** (WebKit has no native CSS scroll anchoring): outer `pinned`/`distanceFromBottom`/`lastScrollHeight` + `reanchor()`, driven by both a data path (`scrollSignal` effect) and a layout path (a `ResizeObserver` on the content element), plus per-live-region inner pins (`liveScroll` action) and per-clipped-preview `ResizeObserver`s (`measureClip`). This machinery is delicate, well-commented, and covered by the new browser suite. **Any change to transcript rendering must preserve it, not replace it** — which is why this plan prefers CSS containment (M3) over a JS virtualizer (M4) unless measurement says otherwise.
- **A real-WebKit test layer now exists** (`tests/browser/*.browser.test.ts`, `make test-browser`, in CI via `make check`) with mount/seed helpers (`tests/browser/harness.ts`, `fixtures.ts`) and geometry-polling conventions. All layout-coupled verification in this plan uses it.
- `Textarea.svelte`, `composeStore.ts`, and the compose autosize/persist paths are **untouched** by #27–#31 — the diagnosis above still holds verbatim.

### Required reading before implementing

- This codebase: `AGENTS.md` (test-type vocabulary, browser-test conventions), `docs/implementation_plans/2026-05-27-transcript-preview-collapse.md` (the compact-mode contract, including its "Future virtualization compatibility" section), `docs/implementation_plans/2026-06-08-webkit-component-tests.md`.
- Layout thrashing / forced synchronous layout: https://web.dev/articles/avoid-large-complex-layouts-and-layout-thrashing
- CSS containment + `content-visibility`: https://developer.mozilla.org/en-US/docs/Web/CSS/content-visibility and https://web.dev/articles/content-visibility (M3)
- `contain-intrinsic-size`: https://developer.mozilla.org/en-US/docs/Web/CSS/contain-intrinsic-size (M3)
- `field-sizing`: https://developer.mozilla.org/en-US/docs/Web/CSS/field-sizing (background for M1's future note — explicitly out of scope)
- TanStack Virtual (only if M4 triggers): https://tanstack.com/virtual/latest

## Shared conventions for all milestones

- **Layout facts are verified in the browser suite** (`*.browser.test.ts`, real WebKit), polling measured geometry (`expect.poll` / `expect.element`) — never fixed sleeps. Logic-only behavior goes in the fast jsdom suite. Follow the mock-surface and reset patterns documented at the top of `tests/browser/harness.ts`.
- **Latency numbers are gathered manually, not asserted in CI.** Wall-clock input-latency assertions are flaky. The measurement protocol (M3 establishes it; M1 reuses a lighter version) is documented and its results recorded in the PR description. CI asserts *behavioral invariants* only.
- **Rationale survives into code.** The non-obvious decisions here — why the resize path is structured the way it is, why persistence has explicit flush points, why containment was chosen over virtualization — must land as comments at the relevant sites (matching the codebase's comment style: the *why*, not the chronology). The diagnosis in this doc is the reference; code comments point at behavior, not at this plan.
- Each milestone is complete (code + tests + docs) and reviewed by a human before the next begins. No commits until approved. When an assumption turns out load-bearing and unverified, stop and ask.

---

## Milestone 1 — De-thrash the compose textarea autosize

### Goal & outcome

Make typing in the compose bar do the minimum possible layout work per keystroke, independent of any transcript change.

- A keystroke triggers **at most one** forced layout (down from two), with no per-keystroke `getComputedStyle` call.
- Autosize behavior is visibly unchanged for **every** `autosize` consumer — the compose bar (cap `max-h-48`) *and* the prompt composer's two inputs (cap `max-h-40`): grows with content, caps at the instance's own max-height, shows an internal scrollbar past the cap, shrinks on delete, resets on send-clear.

### Implementation outline

All changes in `src/lib/components/ui/Textarea.svelte`. It has **three** `autosize` consumers across two components: the compose bar (`ComposeBar.svelte`, cap `max-h-48`) and the prompt composer's argument + appended-text inputs (`PromptComposer.svelte`, cap `max-h-40`, inside their own scroll container). Every change below alters all three — the differing caps are why the cache must be per-instance.

1. **Single resize path.** Remove the duplicate: today both `handleInput` and the `$effect` on `value` call `resizeToContent` for the same change. Keep the `$effect` (it also covers programmatic changes — send-clear setting `draft = ""`, draft restoration) and drop the resize from `handleInput`. One forced layout per value change instead of two.
2. **Cache the max-height per instance.** Each instance's cap comes from a static class; read it lazily on first resize, **per component instance** (never module-level — the caps differ across consumers), instead of `getComputedStyle` per keystroke. Note in a comment that the cache assumes an instance's cap doesn't change at runtime — if a future caller varies it, recompute on class change.
3. **Contingent — coalesce via `requestAnimationFrame`.** Only if M1's own manual measurement still shows layout cost attributable to interleaved write→read across components *after* steps 1–2: schedule the measure/write in rAF (with an unmount guard). Do not build this pre-emptively — with the duplicate resize gone, Svelte already coalesces same-flush changes into one effect run, and typing-driven and streaming-driven flushes are separate tasks, so the cross-component interleaving this would guard against is unmeasured.

**Future note (deliberately not in scope):** `field-sizing: content` would let CSS do the sizing and the JS path be deleted — but the app renders in the *user's* system WKWebView, so adoption is gated on the **minimum supported** WebKit shipping the feature (a fleet fact), never on a dev-machine `CSS.supports` probe: deleting the JS fallback on a dev-machine "yes" would silently freeze the compose bar at minimum height for every user on an older macOS. No minimum macOS floor is declared today (`tauri.conf.json` has no `minimumSystemVersion`); that floor decision belongs to the macOS release/distribution work (`docs/implementation_plans/2026-05-30-macos-release-distribution.md`), not a perf PR. Revisit when a floor exists and includes a `field-sizing`-capable WebKit.

What this milestone does **not** claim: the one remaining forced layout is still O(document). M3/M4 is what makes that layout cheap. State this in the component comment so nobody later "finishes" the optimization by deleting the resize entirely.

### Definition of done

- jsdom component tests: one resize per value change (spy on the measure path), resize fires on programmatic clear (send path), no resize when `autosize` is false, and — guarding the per-instance cache — two concurrently-mounted autosize textareas with different caps resize independently (this fails specifically on the module-level-cache mistake).
- Browser-suite tests (real layout, poll geometry): seeded multi-line value grows the textarea; past-cap content caps the height and sets internal overflow; deleting shrinks it — for the compose bar **and** for a prompt-composer textarea at its own `max-h-40` cap.
- Manual before/after check — **deferred into M3's protocol** (decided at M1 review): the ad-hoc "real long project" version produces an anecdotal number that can't be compared to M3's controlled numbers anyway, and M3's seeded fixture + written step-by-step recipe make the measurement reproducible. Consequence: the contingent rAF step (M1 step 3) stays undecided until the M3 measurement runs. Everything else in this DoD is complete.

---

## Milestone 2 — Debounce compose-draft persistence

### Goal & outcome

Remove the synchronous serialize+`localStorage.setItem` from the per-keystroke path without weakening any of the durability guarantees the current synchronous design exists for.

- Typing performs no `localStorage` write per keystroke; writes coalesce behind a short trailing debounce (~200 ms).
- A draft still survives: project switch, compose-bar unmount, and send-clear ordering (a send must never resurrect just-sent text). App quit is best-effort — see the accepted-loss decision in the DoD.

### Implementation outline

`src/lib/state/composeStore.ts` (+ touchpoints in `ComposeBar.svelte`).

The current module comment is explicit about why writes are synchronous: a deferred write could race a send-clear (resurrecting sent text) or a project switch (writing one project's draft into another's slot). The debounced design keeps those guarantees by making flush points explicit rather than relying on timing:

1. `persist()` becomes debounced (trailing, ~200 ms). Add an exported `flush()` that runs any pending write immediately.
2. **Flush points** (each one exists to kill a specific race — comment them as such):
   - `persistContentNow()` in `ComposeBar` (the send-clear path) already writes through explicitly — make it call the immediate path, bypassing/cancelling the pending debounce so a stale pre-send draft can't land after the clear.
   - Compose-bar `onDestroy` (covers project switch — the bar is remounted per project via `{#key}`).
   - A `window` `pagehide`/`beforeunload` listener registered once by the store (covers app quit mid-debounce).
3. The per-project slot-isolation race ("writing one project's draft into another's slot") is structurally avoided because `setContent` updates the in-memory `store` synchronously — only the *serialization* is deferred. The debounced write always serializes current state. Make this invariant explicit in the comment: **mutations stay synchronous; only persistence defers.**

### Definition of done

- Unit tests (jsdom, fake timers): N rapid `setContent` calls → one `setItem`; `flush()` writes immediately; destroy-flush preserves a draft across a simulated remount (the existing `_testing.reloadFromStorage` restart-path pattern); send-clear followed by debounce expiry never resurrects the cleared draft; `pagehide` flushes.
- Quit-mid-debounce loss is **accepted deliberately** (decided at M2 review): everything typed since the last ≥200 ms pause or flush (a trailing debounce never fires during continuous typing, so the loss is *not* bounded at 200 ms of keystrokes — a non-stop burst is lost whole). Triggered only when the app quits within ~200 ms of the last keystroke *and* neither quit event fires during teardown. Drafts are ergonomic, not load-bearing, and the common exits (send-clear, project switch) flush synchronously and are tested. The passive `pagehide`/`beforeunload` listeners ship as best-effort insurance; their delivery during Tauri webview teardown is intentionally unverified — no `visibilitychange` listener, no Tauri close-hook. (An optional ~30 s probe — type, ⌘Q fast, relaunch, check the draft — would convert "unverified" into a known answer; it gates nothing.)
- Update the module-top comment: it currently documents "writes are synchronous (no debounce)" with rationale — rewrite it to document the new contract (synchronous mutation, debounced persistence, enumerated flush points and the race each one closes).

---

## Milestone 3 — Large-transcript baseline + CSS containment spike (decision gate)

### Goal & outcome

Quantify the remaining typing cost against a controlled large transcript, then attempt to bound transcript layout cost with **native CSS containment** (`content-visibility: auto`) — the approach that preserves PR #31's hand-built scroll-anchoring machinery intact. Ends with an explicit go/no-go on M4.

- A reproducible way to load the dev app (and browser suite) with a parametrically large transcript exists.
- A documented manual profiling protocol exists, with recorded before/after numbers.
- Off-screen transcript blocks are skipped for layout/paint via `content-visibility: auto` + `contain-intrinsic-size`, with no regression in the existing scroll/anchor/compact behaviors.
- **Decision gate:** if per-keystroke layout cost on the large fixture drops to roughly small-transcript levels and the browser suite stays green, M4 is cancelled (record that in this doc). Otherwise M4 proceeds, carrying the spike's findings.

### Implementation outline

> **Note for the reviewer (not the implementer):** the containment approach was identified *after* the original discussion, which had settled on JS virtualization. It is proposed here because PR #31 changed the trade-off: the transcript now contains bespoke re-anchoring + inner pins + per-preview observers that a JS virtualizer would have to subsume or fight, while containment leaves all of it untouched. M4 remains in the plan as the fallback. If you'd rather go straight to virtualization, strike this milestone.

1. **Seeding.** Reuse `tests/browser/fixtures.ts` builders to generate a large mixed transcript (hundreds of turns; text, code fences for Prism mass, tool calls, a fan-out or two). Expose it both to browser specs and to the dev app (a dev-only seeding hook is acceptable; keep it out of production builds the way `DevIndicator` self-gates).
2. **Protocol.** Document (in this file, under M3 results): WebKit Web Inspector → Timelines, type a fixed phrase into the compose bar at natural speed, record per-keystroke Layout duration and total main-thread time; repeat on {small, large} × {before, after}. Also record with compact mode off (worst case) and on (default). Write the steps so a non-WebKit-expert can follow them mechanically (which menu, which tab, what to read off) — this protocol also retroactively satisfies M1's deferred before/after check, so its "before" runs from `main` (`make dev DEV_PORT=…` for side-by-side). Add one **typing-while-streaming** variant (same phrase, typed while a long mock response streams into the large fixture): the user-felt slowdown during streaming mixes the per-keystroke layout tax (which M1/M3 attack) with streaming-pipeline work (reducer, markdown re-parse, re-anchor — out of scope here, see "What it is NOT"), and this variant is what says how much of the felt lag each bucket owns, i.e. whether a future streaming-focused effort is warranted.
3. **Containment.** Apply `content-visibility: auto` with a `contain-intrinsic-size` estimate to each transcript block (the direct children of the `{#each blocks}` container). Off-screen blocks then contribute placeholder geometry instead of full layout, which is exactly what the textarea's forced reflow pays for.
4. **Verify the interactions** the spike exists to de-risk — each has a browser-suite answer:
   - **Re-anchoring:** `reanchor()` reads/writes `container.scrollHeight`/`scrollTop`; with containment, heights of off-screen blocks are estimates. The existing `scroll-hold` / `streaming-pin` / `footer-anchor` browser tests must pass unmodified against the large fixture. Watch for scrollbar jitter as blocks materialize while scrolling up through history (intrinsic-size estimate quality); if jittery, derive better per-block estimates (e.g. from compact-clip height for collapsed units).
   - **`measureClip` observers:** a `ResizeObserver` on a layout-skipped subtree won't fire meaningfully. Acceptable while off-screen (the toggle isn't visible anyway) — but verify it fires correctly when the block scrolls into view, so toggles appear. The `transcript-overflow` and `user-recollapse` tests, run against the large fixture with the unit placed off-screen then scrolled to, cover this.
   - **Mask-image fades + containment** on the same element: verify no paint artifacts (manual + the overflow test).
   - **Streaming, both sides:** while followed (pinned), the live block is on-screen and must never be layout-skipped — confirm the pinned path is unaffected. And the *unfollowed* side: scroll away from an actively streaming live-capped unit so it leaves the viewport (becomes skip-eligible while still growing), let it stream, then scroll back — its inner bottom-pin must still be engaged, top-fade state correct, no content missing. This exercises containment + ResizeObserver-driven growth + the `liveScroll` per-instance pin closure at once. (Completed units render flat — `liveScroll` exists only under `streaming` — so this is the only inner-scroll case.)
5. **Measure after.** Same protocol. Record numbers in this doc under a `### M3 results` heading, plus the go/no-go decision and rationale. Record the exact WebKit/Safari version measured on: `content-visibility: auto` requires Safari-18+ engines (older system WebKits ignore it — a graceful no-op, no breakage, but no perf win either), and Safari 18.0–18.2 had real `content-visibility` rendering bugs (fixed in 18.3), so the spike's findings are engine-version-sensitive in ways one test machine won't show.

Why this ordering (containment before virtualization) — record this rationale in the code comment on the containment class: containment is ~CSS-only, preserves DOM (copy, selection, accessibility, future find-in-page), keeps every PR-#31 behavior and test intact, and is removable in one line if M4 supersedes it. Virtualization is strictly more powerful (DOM count actually shrinks, helping non-layout costs too) but must reimplement the anchoring contract.

### Definition of done

- Seeding fixture + dev hook merged; profiling protocol + before/after numbers + the gate decision recorded in this document.
- Existing browser suite green against the large fixture; new browser test(s) for the off-screen→on-screen `measureClip` case.
- `AGENTS.md` untouched (this is not a new convention — it's an implementation detail of the transcript; the browser-suite section already covers how to test it).

### M3 measurement protocol

Mechanical steps — no profiler experience assumed. This run also retroactively satisfies M1's deferred before/after check and decides M1's contingent rAF step.

**Setup**

1. **"After" app:** on this branch, `make dev`. Open (or create) a project with at least one agent — use two agents if you want fan-out blocks in the fixture.
2. **"Before" app (for the vs-`main` comparison):** in a second worktree on `main`, cherry-pick the seeding commit from this branch: `git cherry-pick 164ff50` ("Add large-transcript dev seeding hook (M3)" — touches only `src/lib/dev/` + one App.svelte hook, so it applies cleanly and changes no rendering). Then `make dev DEV_PORT=1421`. The two apps run side by side with isolated dev configs.
3. **Seed** in each app: click the app window once, press **⌃⌥⇧S** (Control-Option-Shift-S). The transcript fills with ~600 synthetic turns (the Inspector console logs `[dev-seed] prepended …`). Unseeded = the "small" variant; seeded = "large". Repeat presses on an already-seeded project are a no-op.
4. **Scroll-jitter check (manual-only — CI structurally can't see this):** right after seeding, slowly drag the scrollbar from the bottom to the top through the never-rendered history. Watch the scrollbar thumb for jump-backs and the content for visible shifts as blocks materialize. Do it twice: compact mode **on** (default) and **off** — the per-block height estimates target the compact default, so compact-off is expected to be the noisier pass. Record observed/none for each in the results notes.
5. **Paint check (manual):** while scrolled mid-history, confirm (a) the clipped previews' bottom fade masks render correctly; (b) hover-revealed meta/copy chrome stays inside its block; (c) tooltips still overlay correctly (they portal out of the blocks; anything that *didn't* portal would now be clipped by the blocks' paint containment).

**Recording one measurement**

1. Right-click anywhere in the app → **Inspect Element** (dev builds; alternatively Safari → Develop → *your Mac* → *Switchboard*). In the Web Inspector pick the **Timelines** tab.
2. Make sure the **Layout & Rendering** and **JavaScript & Events** timelines are listed (the + button top-left adds them).
3. Click the red **record** button, click into the compose bar, and type `the quick brown fox jumps over the lazy dog` (43 keystrokes) at your natural speed. Stop recording.
4. Drag-select the typing range in the timeline overview. Read off and record:
   - **Layout**: the summed duration and count of Layout events (Layout & Rendering row) → divide duration by 43 for per-keystroke layout cost; note the single longest Layout event.
   - **Main thread**: total time in the selected range (the CPU/summary readout).

**The matrix** — record each cell in the results table below:

- {small, large} × {before (`main`+seed), after (this branch)} with compact mode **on** (the default).
- Large × after with compact mode **off** (the worst-case DOM: the compact toggle button in the transcript toolbar; if per-unit overrides exist it first shows "Reset compact transcript" — click twice).
- **Typing-while-streaming** (large × after): send a real prompt to one agent that yields a long streamed answer (e.g. "write a 400-word summary of how HTTP caching works"), and record while typing the same phrase during the stream. This cell splits the user-felt streaming lag into the per-keystroke layout tax (which M1/M3 attack) vs streaming-pipeline work (out of scope here — see "What it is NOT"); a large residue in this cell with small layout numbers is the signal a future streaming-focused effort is warranted.

**Also record:** macOS version, and the WebKit/Safari version (Safari → About Safari, or `navigator.userAgent` in the Inspector console). `content-visibility: auto` needs a Safari-18+ engine; 18.0–18.2 had real rendering bugs (fixed in 18.3).

### M3 results

*(fill in after running the protocol)*

| variant | layout/keystroke (ms) | layout count | longest layout (ms) | main thread total (ms) |
| --- | --- | --- | --- | --- |
| small × before | | | | |
| small × after | | | | |
| large × before | | | | |
| large × after (compact on) | | | | |
| large × after (compact off) | | | | |
| large × after, typing-while-streaming | | | | |

- Scroll-jitter check (compact on / off): ____ / ____.
- Paint check (masks, in-block chrome, tooltips): ____.
- Environment: macOS ____, WebKit/Safari ____. Note: the "after" cells were measured with the per-kind intrinsic-size estimates already tuned (user ~4rem / agent ~16rem / fan-out ~20rem), so a clean jitter result validates the tuned estimates — there was never a flat-estimate "after" build.
- **M1 rAF contingency** (triggered only if residual cross-component interleaving cost shows after M1+M3): ____.
- **Gate decision (M4 go/no-go):** ____ — *(cancel M4 if large×after ≈ small×after and the browser suite is green; otherwise M4 proceeds carrying these findings).*

---

## Milestone 4 — Windowed virtualization (contingent: only if M3's gate fails — expected NOT to run)

**Status (2026-06-10): on track to be cancelled.** Hands-on testing after M1–M3 found typing on a large idle transcript fast and normal — the symptom this milestone existed to fix if containment fell short. Containment did not fall short, so M4's trigger condition looks unmet; the formal cancellation waits only on the M3 protocol numbers landing in the results table (the gate closes on numbers, not impressions). Two clarifications so this milestone isn't revived for the wrong reason:

- **Typing-while-streaming lag does NOT trigger M4.** The streaming block stays mounted under virtualization and the per-chunk JS pipeline runs regardless — that symptom is M5's, with its own mechanism and fixes.
- **The one realistic remaining argument for M4 is project-switch latency** (see "Recorded but unscoped" below): virtualization would fix it as a side effect by making switch cost O(viewport). But the cheaper middle-tier candidates named there should be priced first — M4 is the most invasive possible answer to that symptom, rewriting the PR-#31 scroll layer to solve a problem two targeted caches might solve alone.

If M4 is ever revived, the outline below remains the agreed constraints; its risk profile (delicate scroll-layer rework, sporadic scroll-position-dependent failure modes, mitigated by the browser suite and compact mode's bounded heights) was assessed in review on 2026-06-10.

### Goal & outcome

If containment can't bound the cost, window the transcript so off-screen blocks are not in the DOM at all. This is a significant rework of `UnifiedTranscript`'s rendering and scroll layer — it requires its own design review against M3's findings before implementation starts; treat the outline below as the agreed constraints, not a finished design.

- Only viewport-adjacent blocks (plus buffer) are mounted; DOM size is O(viewport), not O(history).
- All PR-#31 behaviors hold: outer pin/hold re-anchoring, inner live-region pins, compact previews + overrides, fan-out columns, hidden-item indicators.
- The browser suite — the executable spec of those behaviors — passes, extended to run against the M3 large fixture.

### Implementation outline (constraints carried from prior discussion + #31)

- Virtualize at the **block** level over the existing `blocks` array (`unified.ts` is untouched). Compact-state keys are already data-derived ("virtualization-safe keys" — `transcriptPreview.svelte.ts` documents this contract), so override state survives unmounting.
- **Dynamic measurement is mandatory** (block heights vary by orders of magnitude and change at runtime: streaming growth, expand/collapse toggles, live-cap removal on completion). Evaluate `@tanstack/virtual` against a hand-rolled `ResizeObserver` windower *specifically* for compatibility with the existing `reanchor()` contract — the deciding criterion is which lets the pin/hold semantics (`pinned`, `distanceFromBottom`, the `lastScrollHeight` user-vs-content scroll discrimination) be expressed faithfully. The library is preferred only if it doesn't force replacing that model wholesale. Use the repo's dependency policy (`pnpm add`, lockfile committed) if adding it.
- The outer anchoring rewires to virtualizer terms (scroll-to-end / total-size), keeping the same observable semantics; the **inner** `liveScroll` pins and `measureClip` are per-block and keep working as-is on mounted blocks — but `clipOverflow`'s "entries survive unmount" note and the "one observer per preview… revisit with a shared observer if virtualization lands" note in `UnifiedTranscript.svelte` come due: consolidate to a shared observer keyed by preview key.
- Mount lands anchored at the bottom without a visible top-to-bottom jump (project switch remounts the component).
- The compact "latest completed response" default is computed from data, never DOM presence (already true — keep it that way).
- Known product trade-offs to surface at review time, not silently accept: native text selection across the whole transcript, and any future find-in-page, only see mounted blocks.

### Definition of done

- Full jsdom + browser suites green, the latter extended to the large fixture (streaming-pin, scroll-hold, re-collapse, overflow, footer-anchor all re-validated under windowing).
- New browser tests: bottom-anchored mount; pinned follow during streaming with off-screen history; hold-position on expand/collapse of a mid-viewport block; scroll far up and back (estimate stability).
- M3 protocol re-run; numbers recorded here.
- `clipOverflow`/observer consolidation done; the two PR-#31 comments that anticipated virtualization updated.

---

## Milestone 5 — Streaming pipeline cost (typing-while-streaming lag)

**Trigger (2026-06-10):** hands-on testing after M1–M3 confirmed the original diagnosis split cleanly — typing while agents are idle is fixed; typing while a response streams still lags. That matches the problem statement's "Not streaming-only" analysis: streaming lag is a *throughput* problem (a per-chunk pipeline competing with keystrokes for the main thread), not the layout problem M1–M3 solved. Neither M3's containment (the streaming block is on-screen, so never skipped) nor M4's virtualization (the streaming block stays mounted; the per-chunk JS runs regardless) addresses it — M5 is its own workstream, and **streaming lag is not an argument for triggering M4**.

### Goal & outcome

Typing while an agent streams feels like typing while idle.

- No per-chunk work scales with conversation history (today the reducer map, `buildUnifiedRows`/`groupRenderBlocks`, and `scrollSignal` are all O(transcript) per chunk).
- The pipeline runs at most once per displayed frame regardless of how fast chunks arrive from any harness.
- Streaming rendering behavior is visibly unchanged (live text appears promptly, pin/anchor behavior identical), except where a Fix-3 variant deliberately trades live formatting — that trade is decided explicitly, not slipped in.

### Implementation outline

**Step 0 — profile first (mandatory, before any fix).** Run the M3 protocol's typing-while-streaming cell with the Timelines breakdown split into Layout vs Script, and within Script attribute time across: markdown parse/highlight (`renderMarkdown`), rows rebuild (`buildUnifiedRows`/`groupRenderBlocks`), reducer application, and re-anchor work. PR #28 reworked the reducer path after the original cost analysis, so any pre-profile ranking is stale by construction. Record the breakdown in the M5 results table; it gates Fixes 3 and 4.

The four candidate fixes, in order, with their epistemic status stated so future readers know which were principle-driven and which were measurement-gated:

1. **Coalesce event application to once per frame** — *unconditional: sound regardless of measurement.* Events currently drive state mutation directly from the wire; the display cannot show more than ~60 updates/sec, so pipeline runs beyond that are provably wasted. Batch incoming per-agent events at the single listener seam (`index.svelte.ts`) and apply once per animation frame. This also makes UI cost independent of any future harness's stream chattiness. Constraints: the batch applies FIFO through the existing reducers (batch raw events; never merge them); lifecycle events (`turn_start`/`turn_end`/failure/cancel) either flush the buffer immediately or land with proven ordering — a turn that ends and is cancelled in the same frame is the edge to test. Expect most of the effort in test plumbing: jsdom suites that fire an event and `await tick()` need a flush helper or fake-rAF.
2. **Replace `scrollSignal`'s content walk with a revision counter** — *unconditional: design hygiene with a modest win.* The derived's job is "change signal"; it is implemented as a full digest (thousands of reactive-proxy reads per chunk). Bump a counter wherever `transcripts[agentId]` is assigned. Must provably change for every case the walk caught (text growth, tool output, `completed_at`, row count) — assignment-level bumping covers all. Preserves the jsdom-testability property the existing comment documents (the ResizeObserver path is inert under jsdom).
3. **Stop re-parsing the live segment's markdown per chunk** — *gated on the post-Fix-1 profile.* The problem is pre-registered in `Markdown.svelte`'s own comment, including its named fallback. Variants, in escalation order: **(b)** rely on Fix 1's frame cap (free; each frame still parses the whole segment — may suffice); **(a)** render the live segment as plain text while streaming, full render on completion (the pre-named fallback; simple; visible UX change — no live formatting); **(c)** incremental block-level parsing with a stable-prefix cache (keeps live formatting, architecturally the best end state, but the most complex — unclosed-fence boundary detection is the classic trap, and it adds bug surface to the most user-visible render path). **The (a)-vs-(c) choice is the engineer's** (UX-vs-complexity), made only if profiling shows (b) insufficient; record the decision here.
4. **Incremental row building** — *last resort, profile-gated; do not build speculatively.* The wholesale `buildUnifiedRows` rebuild is a deliberate design (pure, stateless, trivially testable); making it incremental trades that purity for cache invalidation, and the bug class it risks (duplicated/missing/misgrouped rows) is exactly what PR #28 fixed at another layer. Only justified if the profile shows the rebuild dominates *after* Fix 1 caps its frequency. If it does, prefer the middle option first: memoize per-agent sub-results so unchanged agents' rows are reused, keeping most of the purity.

### Definition of done

- Fixes 1–2 landed; Fix 3/4 status recorded in the results below (which variant, or "not needed" with the profile numbers that justified it).
- Coalescing tests (jsdom, fake-rAF): N chunk events in one frame → state identical to sequential application; ordering preserved across mixed event types; `turn_end` (and end+cancel in one frame) never dropped or misordered; events for two agents in one frame both apply.
- Counter tests: every event type that previously moved `scrollSignal` still triggers the re-anchor effect (assert via effect-run observation, not implementation internals).
- Existing jsdom + browser suites green — streaming-pin and scroll-hold specifically re-validated, since coalescing changes *when* re-anchors happen.
- Step-0 and post-fix profile numbers recorded below; rationale comments at the batch seam and the counter site per the plan's rationale-survival convention.

### M5 results

*(fill in: step-0 breakdown, post-fix re-measurement, Fix-3 decision)*

| measurement | layout/frame (ms) | script/frame (ms) | dominant script stage | typing feel |
| --- | --- | --- | --- | --- |
| step 0 — before fixes | | | | |
| after Fixes 1–2 | | | | |
| after Fix 3 (variant: ___) | | | | |

---

## Recorded but unscoped: project-switch latency on large transcripts

Observed 2026-06-10 alongside the M5 trigger: switching back to an already-cached large project shows "Loading project…" for a noticeable time. The data is cached (session-sticky hydration guards); the wait is render-side — Svelte rebuilds the full transcript DOM and re-runs `renderMarkdown` for every text segment on every switch, which containment cannot skip (`content-visibility` skips layout/paint, not component construction or parsing). Not scoped to a milestone. If it warrants one later, the middle-tier candidates (cheaper than M4): cache parsed markdown HTML across mounts keyed by segment text, and/or defer off-screen component mounting. M4 would also solve it as a side effect — this symptom, not typing, is the realistic remaining argument for M4.
