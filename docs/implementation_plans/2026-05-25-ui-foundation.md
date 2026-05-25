# UI foundation & design system

**Status:** proposed plan, awaiting review. **Sequencing:** lands after M4.6 (the project switcher) and **before** the UI-heavier milestones (M4.7 fan-out grouping, M5 slash-command/prompt UI, M6/M7 workflow UI). Establishing the visual language and component primitives now is materially cheaper than retrofitting them through those milestones — every screen we build otherwise encodes ad-hoc styling we'd have to unwind.

**Where it lands:** folded onto the current M4 branch (`m4-dispatcher-contention-cancel`), shipping inside the M4 PR. M4 isn't merged yet and every screen this work refactors lives only on that branch, so a separate branch off `main` isn't cleanly available. Switchboard is pre-launch — scope/PR-boundary purity is not a concern here; treat this as a sidequest within M4.

## Goal & scope

Establish Switchboard's **applied in-app visual language** and a **reusable component system**, then refactor the existing screens onto it — so all subsequent UI inherits a consistent, polished foundation and we polish continuously rather than deferring a big-bang pass. This is explicitly *not* an M8 concern: M8 is operational polish + distribution; the *look-and-feel* has no other home and is cheapest to decide now.

**In scope:** design tokens (light + dark), the semantic color model, typography, spacing/density, the component primitive set (shadcn-svelte-based), refactoring current screens, and a conventions doc that becomes the pattern for M4.7+.

**Out of scope:** a literal logo / marketing brand / app icon (a separate, later effort). "Branding" here means the *applied identity* — how the app looks and feels — not marketing assets.

### Design direction (decided with the maintainer)

- **Aesthetic:** "calm utility" — restrained, dense-but-uncluttered, native-macOS-feeling (Linear / Zed / Claude-desktop lineage). The UI recedes; **color carries meaning** (agent/harness identity, run state), never decoration. Surface-and-border based over heavy shadows.
- **Light + dark, both, from day one.** Tokens are the single source of truth (CSS custom properties); a later component never hardcodes a hex. Follow system preference; a manual override toggle is a small add (see M-UI.1 DoD).
- **Typography:** system UI stack for chrome; system monospace (SF Mono et al.) for code, identifiers, and agent names. Asset-free and native. A bundled branded mono is a deferred one-token upgrade.
- **Color identity:** **monochrome-forward** — primary actions and selection use high-contrast neutral (near-black in light, near-white in dark). A single restrained **teal accent** is reserved narrowly for focus rings, links, and selected-state emphasis. Teal is the one distinct hue left once the four harness colors (Claude≈orange, Codex≈blue, Gemini≈green, Antigravity≈purple) and status colors (amber=processing, red=failed) are accounted for. The accent is one token — trivial to re-tune once seen live.
- **Density:** compact. Small base type, tight spacing scale. **Motion:** minimal (subtle state transitions only).

### Read before implementing

- shadcn-svelte component docs (the project already commits to it; `crates`/`src` use `bits-ui` under `Dialog`). Adopt its primitives rather than inventing.
- Tailwind v4 theming + dark-mode docs (`@theme`, `@custom-variant dark`). The current `src/app.css` is a bare `@import "tailwindcss"` with a `:root` font-family and no token layer or dark wiring yet — M-UI.1 adds that.

## The load-bearing shared pattern: a semantic token model

Every milestone below depends on this, so it's established first and stated explicitly. Components reference **semantic** tokens, never raw palette values or one-off Tailwind colors:

- **Neutral surfaces** — a layered scale (app background → panel → raised → border → text-muted → text). Light and dark are two mappings of the *same* semantic names.
- **Accent** — `accent` (teal) + its on-accent text/ring variants. Narrow use only.
- **Status** — `status-idle` (neutral), `status-processing` (amber), `status-failed` (red), `status-cancelled` (neutral, distinct from failed). These replace today's inline `text-amber-700` / `text-red-700` scattered across `Sidebar`, `UnifiedTranscript`, `ComposeBar`.
- **Harness identity** — `harness-claude` / `-codex` / `-gemini` / `-antigravity` (formalizing today's `harnessDisplay.ts` `HARNESS_BADGE_CLASS` + scattered inline classes into tokens).

The rule that propagates forward: **a component that needs a color names a semantic role, not a hue.** This is what makes dark mode "just work" and keeps later milestones from re-litigating palette choices.

---

## M-UI.1 — Tokens + theming infrastructure

### Goal & Outcome

This milestone is **token + theme-store infrastructure only** — it lays the plumbing the rest of the arc builds on. The existing components are _not_ converted here (that's M-UI.4), so dark mode is intentionally only partially applied until then: the token-driven `body` and any new token-using code theme correctly, while the still-hardcoded primitives (`Button`, `Input`, `Dialog`, harness/status maps) stay light until M-UI.4 migrates them. This is fine because nothing ships mid-arc — no user ever sees the half-converted intermediate state.

- The app has a complete semantic token set (neutrals, accent, status, harness) defined once as CSS custom properties, with light and dark mappings, wired into Tailwind v4 via `@theme`.
- A theme store owns the `.dark` class: `light`/`dark` pin a theme, `system` follows the OS preference and re-applies live on OS flips. The chosen mode persists across reloads and applies pre-paint (no startup flash).
- The store exposes the manual-override **API** (`theme.set(light|dark|system)`); the **visible** light/dark/system control lands in M-UI.3 with the AppShell header (no home for it before then).
- Existing screens still render (unchanged in light mode) after the token layer lands; **visually-correct dark mode app-wide is an M-UI.4 outcome**, once the components are on the tokens.

### Implementation Outline

Define the token model in `src/app.css` (or a dedicated imported stylesheet) as CSS variables under `:root` (light) and a `.dark` / `prefers-color-scheme` mapping, exposed to Tailwind through the v4 `@theme` mechanism and a `@custom-variant dark`. The implementing agent reads the current `app.css` + vite/Tailwind setup first. Provide a tiny theme store (`prefers-color-scheme` listener + manual override persisted to the user-global config or localStorage — implementer's choice, kept trivial). The harness color tokens supersede the literals in `harnessDisplay.ts`; keep that module as the name/label/token map.

### Definition of Done

- Token set documented inline; light + dark mappings exist for every semantic name.
- Toggling OS appearance (or calling `theme.set`) re-themes token-driven surfaces with no per-component work; a pre-paint bootstrap in `index.html` prevents a startup flash for dark-mode users.
- The theme store is unit-tested, including the load-bearing branch (live OS change re-applies only while on `system`) and the invalid-persisted-value fallback. The fuller "renders correctly in both themes" check is M-UI.4's, once components are migrated. Record any token that has no dark value as a deliberate exception.

---

## M-UI.2 — Core component primitives

### Goal & Outcome

- A `ui/` primitive set covers the elements currently hand-rolled across screens, all token-driven and theme-correct: `Button` (variants: primary/secondary/ghost/destructive), `Badge`/`Pill` (harness, status, neutral), `DropdownMenu`, `Card`/`Panel`, `StatusDot`, `Spinner`/loading affordance. `Input`/`Textarea`/`Dialog` (already present) are brought onto the tokens.
- Each primitive has the states the app needs (hover/active/disabled/focus-visible) and nothing speculative.

### Implementation Outline

Adopt shadcn-svelte primitives where they exist (`Button`, `DropdownMenu`, `Badge` — the `+`-menu I hand-rolled in `ProjectsSidebar` becomes a real `DropdownMenu`); extract bespoke ones only where used 2–3+ times in the current code (rule of three — `StatusDot`, harness/status `Badge`, `Panel`). Do **not** pre-build components for milestones whose needs we can't yet see (no workflow-graph or prompt-palette primitives now). Variants are driven by tokens from M-UI.1.

### Definition of Done

- Component tests (mount + assert variant/state classes/roles where behavior matters — e.g. `DropdownMenu` open/close + keyboard, `Button` disabled). Visual-only primitives get a light render test, not coverage theater.
- Each primitive renders correctly in light and dark.

---

## M-UI.3 — Layout & composition primitives

### Goal & Outcome

- The recurring structural patterns are componentized: `Sidebar` + `SidebarSection` (header + scroll body), the three-pane `AppShell`, `EmptyState`, and the existing `Banner` folded onto tokens.
- The current 3-pane layout (projects | transcript+compose | agents) is expressed through these, not bespoke flex/border classes duplicated per screen.

### Implementation Outline

Factor the layout scaffolding that App.svelte + the two sidebars currently inline. Keep these thin — composition, not policy (they take children/snippets). The `AppShell` encodes the pane structure + responsive/overflow behavior once.

### Definition of Done

- Existing layout reproduced via the primitives with no visual regression in light mode; dark mode correct.
- Empty/loading/error center states (today ad-hoc divs in App.svelte) go through `EmptyState`.

---

## M-UI.4 — Refactor existing screens onto the system

### Goal & Outcome

- Every current screen renders through the token + primitive system: `ProjectsSidebar`, the agents `Sidebar`, `UnifiedTranscript` (turn rows, tool blocks, status, outcome markers), `ComposeBar`, `CreateAgentForm`, `AddAgentModal`, the new-project modal, banners.
- The app looks consistent and polished in both themes; no inline one-off colors or duplicated structural classes remain.

### Implementation Outline

Migrate screen by screen, replacing inline styling with primitives + semantic tokens. This is also the first real polish pass — spacing, alignment, type hierarchy, density tightened to the design direction. Preserve all existing `data-testid`s and behavior (the M4.x test suites must stay green); this is a presentation refactor, not a behavior change.

### Definition of Done

- All existing frontend tests pass unchanged (or with only selector updates where a primitive renames a wrapper — behavior assertions unchanged).
- Manual verification in `make dev`, light **and** dark: each screen reviewed for consistency. (State explicitly if the GUI can't be run in a given environment.) The class-presence unit tests prove a primitive _requests_ a token, not that it _resolves_ correctly in dark (jsdom has no CSS cascade) — so this dark pass is a **per-primitive checklist** (Button, Badge ×4 harness + ×4 status, StatusDot, DropdownMenu, Input, Textarea, Dialog, destructive Button), not an end-of-milestone vibe-check. Each must be eyeballed in dark.
- Grep shows no remaining raw status/harness color literals (`text-amber-700`, `text-red-700`, `bg-*-100` harness badges) outside the token definitions.

---

## M-UI.5 — Conventions doc

### Goal & Outcome

- A short `docs/` conventions note (or an `AGENTS.md` pointer) captures: the token model, when to reach for which primitive, the color-carries-meaning rule, density/spacing scale, and the "name a semantic role, not a hue" rule — so M4.7+ build on the pattern without re-deriving it.
- Records the deliberate divergence that the theme preference persists in `localStorage` (per-device, non-synced) rather than the git-trackable `config.yaml` where other user-global state lives.

### Implementation Outline

Write it from what M-UI.1–4 established; keep it a reference, not a tutorial. Link from `AGENTS.md` "Coding conventions → TypeScript/Svelte."

### Definition of Done

- Doc exists and matches the shipped tokens/primitives; `AGENTS.md` points to it.

---

## What this plan does not commit to

- A logo, app icon, or marketing brand (separate later effort).
- Animations/transitions beyond subtle state changes.
- Speculative primitives for future milestones (workflow graph, prompt palette) — built when those milestones define their needs.
- A component-explorer/Storybook setup — revisit only if the primitive set grows enough to warrant one.
