# Multi-panel transcript: per-agent visibility and pane targeting

**Status:** proposed, aligned for implementation (revised after design review — see "Rejected alternatives").
**Branch:** `multi-panel-transcript`.

## Goal & scope

Let the user view subsets of a project's agents — one agent, or a named group (e.g. reviewers vs. implementers) — instead of only the single unified stream, and make it unmistakable which agents a draft message will be sent to. Two tiers, built in order:

- **Tier 0 — per-agent show/hide** on the existing single transcript view.
- **Tier 1 — N side-by-side transcript panes** (a 1-D resizable row) that **strictly partition the roster**, with panes acting as send targets.

**In scope:**

- Per-agent visibility toggles (show/hide an agent's turns and its corresponding user messages) with a one-gesture "solo" path.
- An ordered row of 1..N transcript panes with resizable gutters; add / remove / rename panes; move an agent between panes via the agent's action menu. Every agent lives in exactly one pane; the default single pane holding the whole roster *is* today's unified view.
- Pane-as-send-target: clicking a pane's header (or Cmd+click anywhere in it, or `@panename`, or `Cmd+Alt+1..N`) sets the compose recipient set to that pane's members; an accent border on each pane reflects — always derived, never stored — how much of the current recipient set it covers.
- Device-local persistence of pane layout per project (localStorage).

**Out of scope (deliberate, do not build):**

- **The 2-D quadrant grid** (panes spanning cells, drag-to-rearrange drop zones). Everything in this plan is one layout axis; the quadrant grid adds a second axis, cell-spanning, and drop-target geometry — a small layout engine with its own persistence and test matrix. Deferred until the 1-D row proves insufficient in real use.
- **Per-pane compose bars.** A single global compose bar remains. `ComposeBar.svelte` is ~1,400 lines of prompt composer, attachment staging, `@` menu, hydration gating, and draft persistence; instantiating it per pane multiplies all of that state. Rejected in favor of pane targeting (below).
- **Drag-and-drop agent assignment.** The action-menu path is the foundation; drag is a possible later accelerant.
- **Showing an agent in more than one pane** (mirroring/duplication). See "Rejected alternatives."
- Backend, dispatcher, journal, or IPC changes. **This is a pure frontend feature** — dispatch already fans out over a set of agent IDs; nothing below the compose bar changes.
- Transcript virtualization (planned separately; see compatibility note at the end).

## Required reading before implementing

- `docs/ui-conventions.md` — token model (the `accent` token is semantically "focus/selection emphasis" and is what every targeting visual in this plan uses), `ui/` primitives, theming.
- `docs/system-design.md` §7 ("Sends and turns") — the send/turn vocabulary used throughout.
- `docs/implementation_plans/2026-05-27-transcript-preview-collapse.md` — the precedent for a small per-project frontend state module (`transcriptPreview.svelte.ts`) and for virtualization-compatible keying rules.
- `docs/implementation_plans/2026-06-08-webkit-component-tests.md` — the browser-test layer this feature must extend (`tests/browser/`, `*.browser.test.ts`, poll measured geometry, never fixed sleeps).
- `src/lib/state/composeStore.ts` — the persistence pattern Milestone 2 follows (versioned single-key localStorage envelope, per-project map, migration fallback). Read its module doc-comment; the rationale there (device-local, origin-scoped, never git-synced) applies verbatim to pane layout.
- Svelte 5 runes (`$state`/`$derived`): https://svelte.dev/docs/svelte/what-are-runes — every derivation rule in this plan is expressed as `$derived`.

Library note: `paneforge` (resizable panes, same lineage as our `bits-ui` dep) was considered and **rejected** for now — `GitView.svelte` already proves the vanilla pointer-resize pattern (clamped width `$state`, `svelte:window` pointermove/pointerup), and it generalizes to N gutters without a new dependency. Revisit only if the gutter math turns out genuinely painful.

## The pane model

One concept, stated once because everything else derives from it:

**A project's panes are an ordered list that strictly partitions the roster.** Every pane is a named set of agents; every agent belongs to exactly one pane; no agent ever renders in two panes. There is no special "unified" or "all" pane kind — the unified view is simply the default state of **one pane containing every agent**. Splitting *moves* agents out of that pane; merging back is moving them back (or closing panes). The default pane is a pane like any other.

Consequences (each is a rule, not a suggestion):

- **Moving an agent** to another pane removes it from its current pane. "Move," never "copy."
- **Closing a pane** merges its members and hidden-set entries into its **left neighbor**; if the closed pane is leftmost, into its **right neighbor** (which becomes the new leftmost). Close is unavailable while only one pane exists — there is no neighbor to merge into. An agent can never be orphaned — partition is an invariant, not a best effort.
- **New agents** (created or roster-loaded additions not present in saved layout) land in the **leftmost pane** — predictable, and degenerate-case-correct (one pane = today's behavior).
- **An emptied pane stays open** (the user named it; closing is the user's call).
- **Every pane is named from creation.** The default pane is "Pane 1"; new panes default to "Pane N". Single-pane chrome suppression means the default name first becomes visible — and `@`-addressable — only once a second pane exists; there is no rename-on-first-split event.
- **Visibility (the Tier 0 eye/solo) is per-pane**: hiding an agent hides it within the pane it belongs to; solo shows only that agent within its own pane, leaving other panes untouched. Pane-local solo is deliberate — a mixer-style global solo would empty every unrelated pane, which is more disruptive than helpful in a tiled layout, and the "N hidden · Show all" reset self-heals either reading. Membership decides *where* an agent appears; visibility decides *whether* it currently does.

## Design decisions that bind this plan

These came out of design discussion and review and are not recoverable from the code. They must survive into the implementation as `why` comments at the relevant sites (per the repo's comment policy: comments state constraints the code can't show).

1. **`selectedIds` is the single source of truth for "who receives the send."** Panes are a grouping layer that *reads from and writes to* the compose recipient set — never a parallel target state. There is no stored `targetedPaneId`, no stored "docked pane," no cached per-pane recipient list. Every targeting visual is a pure derivation of `selectedIds` ∩ pane membership. Rationale: a second stored representation of the target can drift from the real one (drop one chip and a stored pane id still highlights the whole pane), and a targeting cue that can lie *causes* the mis-sends this feature exists to prevent.
2. **Visibility and targeting are independent axes.** An agent can be hidden-but-receiving or visible-but-not-targeted. Visibility controls therefore live in the agents sidebar / pane chrome, never in the compose bar; the compose bar owns only "who receives."
3. **Reading and targeting are different gestures.** Plain clicks inside a pane body (to scroll, select text, copy) must never re-target the draft. Re-targeting happens only on explicit gestures: pane header click, Cmd+click anywhere in the pane, `@panename`, `Cmd+Alt+1..N`. Rationale: if any click re-targets, clicking into a pane just to copy a line silently re-aims a half-typed draft.
4. **One border, one meaning — and targeting chrome exists only when there is a choice.** There is exactly one pane-level highlight, it means "recipients," and it is tri-state (full / partial / none coverage of `selectedIds`). **All targeting visuals and gestures — coverage borders, the Cmd-held overlay, header-click targeting — are inert while the project has a single pane.** Rationale: with one pane there is nothing to disambiguate, and the chrome would be pure noise in the default state; this gate is also what keeps the no-split default pixel-identical to today's UI (a promise Milestone 2 makes and Milestone 3 must not break). No separate "focused pane" outline exists — there is consequently **no stored pane-focus state at all**; each pane has its own scroll container and the browser handles "which pane am I reading" natively.
5. **Panes strictly partition the roster** — the full model and its consequences are in "The pane model" above; the partition invariant (move-never-copy, merge-on-close, no orphans) is enforced by the state module and is the load-bearing `why` comment site.
6. **No pane chips in the compose bar.** The pane itself is the group affordance (header click selects its members); a chip duplicating that is redundant tiering. The recipient row stays a flat list of agent chips. Keyboard access to panes goes through the `@` menu and `Cmd+Alt+N`.
7. **`@panename` uses replace semantics**, matching today's `@agentname` ("picks one as the sole recipient"): it sets `selectedIds` to exactly the pane's members. Pane-click and `@pane` thus mean the same thing. Pane entries appear in the `@` menu **only when ≥2 panes exist** (with one pane, the existing `all` pseudo-action already covers the only possible pane target; listing both would be duplicate rows for the most common state).
8. **`Cmd+Alt+1..N` targets pane N**, numbered by pane array order, left to right (so `Cmd+Alt+1` is the leftmost pane). `Cmd+1..9` is taken by per-agent chip toggles. `Cmd+Ctrl` conflicts with the user's OS/window-manager bindings. The ComposeBar chord handler explicitly bails when `altKey` is held (`if (!mod || e.altKey) return`), so the `Cmd+Alt` number row is free by construction; the only existing `Cmd+Alt` binding is `Cmd+Alt+B` (App.svelte's global handler, `event.code === "KeyB"` branch).
9. **Cmd+click is a net-new idiom in this app** (verified: every existing `metaKey`/`ctrlKey` site is a keyboard chord, none a modifier-click). It earns its place via the held-modifier overlay (below), which makes it discoverable and previews intent before commit. Do not describe it in code/docs as extending an existing convention.
10. **No "dock" treatment on the compose box — the pane's coverage ring is the only targeting visual.** (Revised after real use.) The original decision gave the compose bar a non-positional accent border whenever `selectedIds` set-equaled one pane's members; it was implemented and then removed: in practice a persistent accent on the compose surface read as unexplained noise rather than a signal, and the targeted pane's own full-coverage ring already says everything the dock said. The earlier sub-decision stands on its own: **positional alignment of any compose-bar cue to a pane (an accent bridge under that pane's horizontal extent) remains rejected** — live geometry coupling (`getBoundingClientRect`/ResizeObserver re-run on every gutter drag and window resize) for a cue the pane border already carries.

## Rejected alternatives (recorded so they aren't re-litigated)

- **A mirror/"all" pane** (a pane kind that always shows the entire roster while members-panes act as additional filtered lenses, so an assigned agent renders in two panes at once). Rejected in design review for the strict partition: the mirror pane needed its own kind discriminant, an "unassigned agents" state, duplication-rendering semantics, and a coverage-border carve-out (its border would sit permanently at "partial" under any subset targeting). The partition deletes all four problems. The trade accepted: there is no simultaneous "everything" view alongside split panes — the unified view is recovered by closing panes or moving agents back.
- **Compose-box "dock" accent** (any persistent accent on the compose box reflecting pane targeting — both the positional bridge and the non-positional border that briefly shipped) — see decision 10.
- **`paneforge`** — see the library note above.

## User experience

### Tier 0 — show/hide (single view)

Each agent card in the right agents sidebar gains an eye toggle:

- **Click** hides/shows that agent: its agent turns disappear from the transcript, and user messages whose recipient set contains *only* hidden agents disappear with them (a message to a mix of hidden and visible agents stays, attributed to the visible ones). This falls out of the existing roster filter — see implementation.
- **Alt/Option-click solos** the agent: show only it, hide all others. Alt-clicking the soloed agent restores all. (Same gesture as soloing a mixer track or a layer.)
- When any agent is hidden, the sidebar's Agents section header shows a **"N hidden · Show all"** reset — mirroring the reset affordance the compact-mode header button already provides.
- If every agent is hidden, the transcript area shows an empty-state hint with the same Show-all action.
- **Targeted-but-hidden cue:** if a compose recipient chip's agent is currently hidden, the chip carries a small warning affordance (tooltip: the reply won't be visible). Without this, a user sends and never sees the response appear.

### Tier 1 — panes

Default state is exactly today's UI: one pane holding the whole roster, with no targeting chrome (decision 4) and minimal/suppressed header chrome. The layout becomes interesting when the user splits:

- **"Move to pane ▸"** submenu on each agent card's existing action menu, listing current panes plus **"New pane"**. Moving an agent to a new pane creates the pane (default name "Pane 2", renamable) and moves the agent there — out of its previous pane (partition; move, never copy).
- Panes render left-to-right in one row, each a full transcript+scroll region showing exactly its members (minus any per-pane eye-hidden agents). N−1 draggable gutters resize adjacent panes; widths clamp to a per-pane minimum (follow GitView's ~360px clamp). "New pane" is unavailable when another pane can't fit at minimum width.
- Pane header: name, rename affordance, member summary (harness icons / count), close button. **Rename follows the sidebar's existing inline agent-rename pattern** (explicit edit affordance opening an inline input — see `agent-rename-input`/`agent-rename-save` in `Sidebar.svelte`); the header text/surface itself is the *target* gesture, so the two affordances never collide. Closing a pane merges its members into the left neighbor (right neighbor for the leftmost pane); the close affordance is absent in the single-pane state.
- **Targeting** (every path writes `selectedIds`; every visual derives from it; all of it inert with a single pane):
  - Click a pane **header** → `selectedIds = pane members`.
  - **Hold Cmd** → the pane under the cursor shows an accent overlay with a "target ⌘" affordance previewing the commit; **Cmd+click anywhere in that pane** → `selectedIds = pane members`. Plain body clicks never re-target.
  - **`@panename`** in the composer → same, via the existing `@` typeahead (pane entries listed ahead of agents; only when ≥2 panes exist, per decision 7).
  - **`Cmd+Alt+1..N`** → same, for pane N (leftmost = 1).
- **Coverage border:** each pane shows an accent border when *all* its members are in `selectedIds`, a visibly distinct partial treatment when *some* are, nothing when none are. Dropping one agent chip from a fully-targeted pane immediately demotes its border to partial — the border cannot disagree with the actual recipient set. (Selecting the whole roster via `@all`/`Cmd+Shift+A` legitimately shows every pane fully covered — truthful, since every pane's members will receive.)
- **The compose box never changes for pane targeting** (decision 10, revised): the targeted pane's coverage ring is the sole visual; the compose box stays neutral regardless of how the recipient set relates to panes.
- Pane layout (panes, names, membership, per-pane hidden sets, widths) persists per project in localStorage and restores on reopen. Rationale for localStorage over `config.yaml`: window arrangement is a personal, per-device preference, not shared project config (see `composeStore.ts`'s module comment for the fuller version of this argument).

## Current-code alignment (what makes this cheap)

The data layer already does the hard part; an implementing agent should internalize this before writing anything:

- `UnifiedTranscript.svelte` is already parameterized by an `agents: AgentRecord[]` prop — it flattens only those agents' `transcripts[agent_id]` and derives the roster set passed to the merge.
- `buildUnifiedRows(turns, overlay, knownAgentIds)` (`src/lib/state/unified.ts`) already filters every row to an agent-id set, **prunes user messages to surviving recipients, and drops messages with no surviving recipient** — written for the removed-agent case, but it is exactly "show only this pane's agents and their user messages." A fan-out filtered down to one visible recipient collapses to a plain single-recipient send automatically.
- Each `UnifiedTranscript` instance owns its scroll anchoring (ResizeObserver re-anchor) and live-pinning internally, so multiple instances compose without coordination.
- Recipient targeting is already a plain `selectedIds: AgentId[]` in `ComposeBar.svelte` (persisted per project via `composeStore`), with chips, `@` typeahead (`recipientItems`), and `Mod+1..9` all operating on it. Pane targeting is sugar over this; dispatch is untouched.

Therefore: **a pane is `<UnifiedTranscript>` handed a filtered roster.** The new work is the state module, the layout shell, and the targeting derivations — all additive; `unified.ts` and the reducers do not change.

---

## Milestone 1 — Pane state module + Tier 0 show/hide

### Goal & outcome

Per-agent visibility on the existing single transcript, plus the state module that Milestone 2 will grow into the full pane model (so Tier 0's state is not throwaway).

Outcomes — once complete the user can:

- Hide/show any agent from its sidebar card (eye toggle); hidden agents' turns and their now-recipientless user messages vanish from the transcript.
- Alt-click an agent to solo it; alt-click again to restore all.
- See "N hidden · Show all" in the sidebar header whenever anything is hidden, and an empty-state hint if everything is.
- See a warning cue on any compose recipient chip whose agent is hidden.

### Implementation outline

**New state module** (e.g. `src/lib/state/transcriptPanes.svelte.ts`, naming up to the agent) — per-project, session-only `$state`, modeled on `transcriptPreview.svelte.ts`. This module is the seed of the full pane model, which is why its shape matters more than Tier 0 alone would justify:

- The state is **an ordered list of panes from day one**, each pane a named set of member agent ids plus a per-pane hidden set. In this milestone there is exactly one pane, holding the whole roster; Tier 0's eye toggle edits that pane's hidden set. Do not build multi-pane machinery beyond what one pane needs (no assignment ops yet); do ensure extending to N panes means adding entries and operations, not reshaping the data.
- Expose the operations the UI needs (toggle visibility, solo, show-all, visible-member derivation) and prune stale agent ids against the roster the same way ComposeBar prunes `selectedIds` when an agent disappears; new agents join the leftmost (here: only) pane.

**Wiring:** `App.svelte`'s active-project block currently passes the full roster to `UnifiedTranscript`; pass the pane's visible-member roster instead. No changes inside `UnifiedTranscript` or `unified.ts` — the existing filter path does the rest (this is the point; resist touching the merge).

**Sidebar:** eye toggle on each agent card (`Sidebar.svelte`); alt-click = solo (branch on `event.altKey` in the same handler); "N hidden · Show all" action in the `SidebarSection` header area next to the existing expand/collapse-all control. Per design decision 2, no visibility controls in the compose bar.

**Compose chip cue:** recipient chips read the visibility state and render a small warning treatment (existing `Tooltip` primitive for the explanation). This is a read-only cross-module dependency — ComposeBar must not write visibility.

Edge cases identified in discussion: hiding all agents (empty state + reset, allowed, not prevented); single-agent projects get no special casing (hiding the only agent is pointless but harmless and the reset is one click); roster changes prune visibility state.

### Definition of done

- jsdom component tests (mock `invoke`/`listen` per AGENTS.md; `await tick()` for presence): toggle hides agent turns AND sole-recipient user messages; mixed-recipient message survives with pruned attribution; solo / un-solo; show-all reset; hidden-recipient chip cue appears and clears; stale agent ids pruned on roster change.
- Unit tests on the state module's operations (toggle/solo/show-all/prune/new-agent-joins-leftmost) — deterministic, no DOM.
- No browser-suite additions needed (no new layout-coupled behavior in this milestone).
- `make check` passes. Stop for human review; do not commit until approved.

## Milestone 2 — Partitioned panes + N-pane layout shell

### Goal & outcome

The single hardcoded transcript becomes an ordered row of 1..N panes partitioning the roster. This milestone deliberately excludes targeting (Milestone 3) so the layout refactor is reviewable on its own.

Outcomes — once complete the user can:

- Move an agent to a new or existing pane from its card's action menu; the agent's transcript (its turns + its user messages) moves with it — it no longer renders in its previous pane.
- Rename a pane (inline edit affordance per the sidebar rename pattern); close a pane (members merge into the left neighbor, or the right neighbor if it was leftmost; close unavailable with a single pane).
- Drag gutters to resize panes, with a sane minimum width; "New pane" is unavailable when it can't fit.
- Quit and reopen the app to the same pane layout per project — including on a narrower window than the layout was saved on.
- See no change at all if they never split: one pane holding everyone is pixel-equivalent to today's UI.

### Implementation outline

**State module growth** (the Milestone 1 module — extend, don't fork): assignment operations enforcing the partition invariant (move removes from prior pane — design decision 5; this is the `why`-comment site), create-pane, close-pane (merge members + hidden-set entries into the left neighbor — right neighbor when the closed pane is leftmost; the operation is unavailable with a single pane), rename. Pane order is array order — it drives `Cmd+Alt+N` numbering and `@` menu ordering in Milestone 3, so it is part of this module's contract. New agents join the leftmost pane.

**Persistence:** follow **`composeStore.ts`'s pattern** — a single localStorage key holding a versioned envelope (`{version, projects: {[projectId]: layout}}`) with try/catch fallback and a migration path; not `theme.svelte.ts`'s single-global-string shape. Persist panes (id/name/members/hidden) and widths. Per-pane hidden sets persisting across sessions is deliberate — hiding is curation, like membership, and the sidebar's "N hidden · Show all" reset keeps a restored hide discoverable rather than mysterious. Restore must tolerate: stale agent ids (prune; re-home roster agents missing from the saved layout into the leftmost pane), a missing/corrupt entry (fall back to the default single pane), and **widths saved on a wider window**: re-apply the live clamp per pane and redistribute when the sum exceeds available width; if even `paneCount × min` cannot fit, degrade predictably (implementer picks proportional-below-min or min-with-clip and records the choice) — **geometry never alters membership**.

**Layout shell — the one real refactor in this plan:** `App.svelte`'s active-project block currently hardcodes one `<UnifiedTranscript>` + `<ComposeBar>`. Replace with: a flex row mapping the pane array → pane components, each wrapping its own `<UnifiedTranscript>` (that pane's visible members), separated by resize gutters; the single global `<ComposeBar>` stays below the row. **Do not special-case "exactly two panes"** — the array-of-panes shape is the deliverable; one pane and four panes are the same code path. Generalize GitView's pointer-resize pattern (clamped width `$state`, `svelte:window` pointermove/up) into a reusable gutter rather than copying it per pane; whether that's a `ui/` primitive or a local component is the agent's call after reading `docs/ui-conventions.md`.

**Pane chrome:** header (name, rename per the sidebar inline-rename pattern, member summary, close). For the single-default-pane case, suppress or minimize chrome so the no-split experience matches today.

**Assignment UI:** "Move to pane ▸" submenu in the agent card's existing `DropdownMenu` (panes + "New pane"). No drag-and-drop (out of scope).

**Min-width policy** (settled in discussion as the one deliberately-designed constraint of N panes): clamp every pane to a minimum (follow GitView's 360px unless the agent finds a better existing constant); disable "New pane" when `(paneCount+1) × min > available width`. No horizontal scroll, no auto-collapse — simplest policy that prevents degenerate layouts.

Edge cases: agent removed from roster while assigned (prune membership; an emptied pane stays open); fan-out send whose recipients span panes renders in each pane filtered to that pane's members (this falls out of `buildUnifiedRows`; add a test, not code); project switch swaps the whole layout (state is per-project).

### Definition of done

- Unit tests: partition invariant (move never duplicates; every roster agent in exactly one pane after any op sequence), close-pane merge into left neighbor (members and hidden entries) **including the leftmost-close case (members land in what was the second pane, now first) and close being unavailable with a single pane**, persistence round-trip including stale-id re-homing, corrupt-entry fallback, version-migration case (mirroring `composeStore`'s), and over-wide-width re-clamp/redistribution; pane-order stability.
- jsdom component tests: move-to-pane menu flow (agent disappears from old pane, appears in new); per-pane content filtering (agent turns + user-message pruning per pane); cross-pane fan-out renders correctly in each pane; rename; close-merges-members.
- **Browser tests** (`tests/browser/`, per the WebKit plan): gutter drag actually resizes (poll measured widths); min-width clamp holds; restore at a narrower window yields widths summing to available width with functional gutters; per-pane scroll anchoring still re-anchors independently with two panes streaming (this is the regression the jsdom suite physically cannot catch).
- The no-split default is visually unchanged from `main` (manual check at review).
- `make check` (including `make test-browser`) passes. Stop for human review; do not commit until approved.

## Milestone 3 — Pane targeting

### Goal & outcome

Panes become send targets, and the recipient set becomes impossible to misread. This is the payoff milestone for the mis-send pain point.

Outcomes — once complete the user can:

- Click a pane's header to target it: the compose recipient set becomes exactly that pane's members.
- Hold Cmd to see a "target" overlay on the pane under the cursor; Cmd+click anywhere in it to target it. Plain clicks in pane bodies never change recipients.
- Type `@<pane name>` to target a pane from the keyboard; press `Cmd+Alt+1..N` to target pane N.
- Read the recipient set off the panes at a glance: full accent border = every member targeted; partial treatment = some; none = none. Dropping an agent chip demotes the border instantly.
- See none of this chrome — borders, overlay — while the project has a single pane: the default UI stays exactly as it is today.

### Implementation outline

All of this milestone is derivation + gesture wiring over `selectedIds`; if any piece wants to store a target, it's wrong (design decision 1 — put the `why` comment on the coverage derivation). Everything below is gated on ≥2 panes (design decision 4 — put that `why` comment on the gate).

**Coverage derivation:** a `$derived` mapping each pane → `"full" | "partial" | "none"` from `selectedIds` ∩ membership. Drives the pane border (accent token per `docs/ui-conventions.md`; partial needs a visibly distinct treatment — e.g. reduced opacity or dashed — agent's visual call within the token system). No compose-box treatment derives from it (decision 10, revised).

**Gestures** (each one *writes* `selectedIds`, full replace):

- Pane header click. The header surface is the target gesture; rename goes through its explicit edit affordance (M2), so the two cannot collide.
- **Cmd-held overlay + Cmd+click:** track Cmd via window keydown/keyup to arm an overlay on the hovered pane ("target ⌘", accent token). On Cmd+click anywhere in the pane: `preventDefault()` and target. Two hazards identified in discussion, both must be handled: (a) **stuck overlay** — the keyup is lost if the app loses focus while Cmd is held (Cmd+Tab away); clear the armed state on `window` blur as well as keyup; (b) verify in the **browser suite** that Cmd+click in the Tauri/WebKit webview has no native side effect and that `preventDefault` suffices.
- **`@pane` entries** in ComposeBar's `recipientItems` derivation, listed ahead of agent entries, replace semantics, **only when ≥2 panes exist** (design decision 7; with one pane the existing `all` pseudo-action — already self-suppressing when everyone is selected — covers the only possible pane target). Pane names share the typeahead namespace with agent names and the `all`/`clear` pseudo-actions; on a name collision the existing menu already disambiguates by listing both — no special handling beyond stable keys.
- **`Cmd+Alt+1..N`:** the natural home is App.svelte's global keydown, which already has the `altKey` branch handling `Cmd+Alt+B` — extend that branch. ComposeBar's own chord handler ignores Alt chords by construction, so no conflict; keep it that way (don't add Alt handling there). Pane numbering = pane array order, leftmost = 1.

**Hidden-recipient cue generalization:** Milestone 1's chip cue ("targeted but hidden") now reads per-pane visibility — a recipient eye-hidden within its own pane gets the same chip treatment and tooltip. (Partition guarantees every agent has a pane, so "in no pane" cannot occur.)

### Definition of done

- Unit tests on the derivations: coverage tri-state across full/partial/none/empty `selectedIds`; whole-roster selection shows every pane full; the compose box stays neutral under pane targeting (decision 10, revised); single-pane project yields no coverage/overlay state at all (the explicit regression test for the unchanged-default promise); pane-order → number mapping.
- jsdom component tests: header click replaces `selectedIds`; chip drop demotes coverage (the "border cannot lie" property — test it explicitly as the core invariant); `@pane` replace semantics, ≥2-pane gating, and menu ordering; `Cmd+Alt+N` targeting incl. no-collision with `Cmd+N` agent toggles and palette-open suppression (mirror the existing chord-guard tests); overlay arms on Cmd-down, disarms on Cmd-up **and on window blur**; plain body click changes nothing; entering pane-rename mode does not change `selectedIds`.
- Browser tests: Cmd+click in real WebKit targets the pane with no native side effects; overlay tracks the hovered pane across two panes.
- README "Harness support and limitations" needs no entry (no harness-facing behavior); update any user-facing shortcut documentation if one exists (check; don't create one).
- `make check` passes. Stop for human review; do not commit until approved.

---

## Future compatibility notes

- **Virtualization** (planned separately): nothing here introduces DOM-position-keyed state. Pane membership and coverage are keyed by agent/pane ids; per-pane scroll anchoring stays inside each `UnifiedTranscript` instance, which is the boundary a future virtualizer will own.
- **Tier 2 (quadrant grid), if ever:** the pane array + membership model is the substrate; the grid adds a layout-assignment layer on top. Nothing in this plan should grow speculative hooks for it.

## Execution rules for the implementing agent

- Milestones in order; each fully done (code + tests + this doc's DoD) before the next. **Stop after each milestone for human review. Never commit without explicit approval**, and never push.
- When a decision in this plan seems to conflict with what you find in the code, or an assumption here turns out wrong, stop and ask — do not improvise a reconciliation.
- The "Design decisions that bind this plan" section must survive into the code as `why` comments at the sites noted; do not let the rationale evaporate into this doc alone.
