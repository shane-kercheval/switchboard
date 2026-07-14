# UI conventions

The durable rules for Switchboard's frontend — the things you can't infer by reading a single file. Per-component prop shapes live in the components' own doc-comments (`src/lib/components/ui/`) and the token list lives in `src/app.css`; this doc deliberately does not restate them, so it can't drift out of sync. The look-and-feel ("calm utility": restrained, dense, native-macOS-feeling, color carries meaning) is described in the UI-foundation plan.

## The one rule: name a semantic role, not a hue

A component that needs a color references a **semantic token**, never a raw palette value or a one-off Tailwind color (`text-amber-700`, `bg-blue-100`). Tokens are defined once in `src/app.css` as CSS custom properties, with light and dark as two mappings of the *same* names, exposed to Tailwind v4 via `@theme inline`. Because the utilities reference the variables by name (not by build-time value), dark mode is "just use the token" — a `.dark`-scoped override re-themes everything with no per-component work.

The token groups (see `app.css` for the exact names/values): **neutral surfaces** (the four-role ramp + interaction fills — see "The neutral ramp" below), **primary** (monochrome high-contrast action/selection), **accent** (a single restrained teal, reserved for links and selected-state emphasis — not decoration, and no longer focus rings), **focus** (the app's one blue — the focus ring and the pale user-input surfaces; see the ramp section), **destructive** / **warning**, **status** (`status-{idle,processing,failed,cancelled}`, each a strong fg + a `-soft` bg), and **syntax** (`syntax-{comment,keyword,string,function,constant,tag}`).

The **syntax** group is the one deliberate exception to the "narrow, meaningful color" rule: legible code highlighting needs several distinguishable hues, more than the neutral/accent/status roles supply. The tokens are named by syntactic role (not hue) and consumed only by `.markdown-body`'s Prism `.token.*` rules — don't reach for them outside rendered Markdown. Like every other token they're two mappings of the same names, so code highlighting flips light/dark with no JS. Each role intentionally covers several Prism token classes (e.g. `syntax-tag` also styles attribute names, properties, and selectors); the authoritative role→class mapping lives in `app.css`'s `.token.*` block — consult it when mapping a newly added language rather than guessing.

Color carries meaning — never use a status hue decoratively, and never introduce a new hue without giving it a named role in `app.css`.

## The neutral ramp

Three fills and one line, each with exactly one job:

| Token | Job |
| --- | --- |
| `surface` | the app shell — the outermost background the rest sits on |
| `raised` | content — the reading pane, cards, popovers, menus, dialogs |
| `panel` | sidebars + recessed / inset — the side panels, code blocks, inputs, expanded tool output |
| `border` | lines only, never a fill |

Note that **sidebars are `panel`, not `surface`** (`SidebarPanel` paints `bg-panel`): they read as recessed side rails against the `raised` content pane, and `surface` is the shell behind everything.

Plus three neutral **interaction** fills — `hover` (the subtle wash under a large row or menu item), `control-hover` (the more visible fill under a compact icon or pill control), and `active` (the strongest interaction step, used for pressed controls, tracks/grooves, and compact actions nested in an already-hovered row) — and the blue `focus` token (below).

Keep the ramp to these. It was once ~15 near-identical grays (several within a percent or two of luminance); the steps were too small to read as hierarchy yet numerous enough to look muddy. Depth comes from *stepping* these few layers, not from adding shades or shadows.

**Match hover strength to target size, not to whichever surface token happens to sit underneath:**

- A **large row or menu item** uses `bg-hover`. Its area makes the subtle wash legible without turning lists into heavy gray blocks.
- A **compact neutral control on `raised` or `surface`** uses `bg-control-hover` through `ICON_BUTTON_CLASS` or its primitive's equivalent.
- A **compact control resting directly on a `panel` sidebar** uses `ICON_BUTTON_ON_PANEL_CLASS` and brightens to `bg-raised`. The same header controls fall back to `ICON_BUTTON_CLASS` when the sidebar is closed and they move onto a white surface.
- An **action icon inside a selectable row** uses `ROW_ACTION_ICON_CLASS`. It uses `bg-active` so its direct hover remains visible inside the row's gray hover; on a selected blue row it switches to `bg-raised`.
- Don't use `bg-active` as a general stronger-hover fallback. Its hover use is limited to nested row actions; otherwise reserve it for pressed/latched states, tracks, grooves, and resize seams.
- **One documented exception:** a row on a `panel` sidebar whose *selected* state is already `raised` (the projects sidebar) can't use `raised` for hover — it would be indistinguishable from selected — so it lightens to `surface`, the off-white step between panel and white. This is the *only* sanctioned use of `surface` as a hover fill; don't generalize it.

**Two banned patterns, both mechanically enforced** by `tests/token-ramp-scan.test.ts`:

1. **No opacity modifier on a surface token** — `bg-{surface,raised,panel,border}/<n>` is out. A translucent fill composes differently over every parent and yields a shade nobody named. Pick the solid role that means what you want.
2. **`border` is a line, never a fill** — `bg-border` in any form is out. A hovered fill is `bg-hover`, a control/track fill is `bg-active`, a divider is an actual `border-*` line.

**A third rule is review-only, not tooled: at most two nested neutral treatments, counting fills and borders together.** A bordered container's child gets a fill *or* nothing, not both — otherwise a borderless design collapses back into nested boxes. A text scan can't count nesting, so this one relies on a human eye in review rather than the test; watch for it.

Blue means exactly one thing: **`focus`**. Inputs, buttons, and menus show it as a thin focus **ring**; the compose box shows it as a **border-color** change (deliberately border-only, to keep that large card's highlight minimal). `focus-soft` is the pale tint for user-authored input surfaces (the user and held-forward message bubbles). It is deliberately blue, not the teal `accent` — a green highlight on a text field reads as *valid*, not *focused*. It shows on actual focus and clears when focus leaves, so it signals *where keyboard focus is* rather than sitting on permanently.

Segmented controls share one color system regardless of size: a `raised` track, `panel` hover for inactive options, and the named neutral `segment-selected` fill for the active option. Standard form controls and compact page-header controls may differ in height and type size, but not in color semantics.

## Reach for a primitive before hand-rolling

Primitives live in `src/lib/components/ui/`. Adopt the existing one rather than re-styling inline; extend a primitive (a new variant/size) rather than forking it. When something is needed in 2–3+ places, extract a primitive (rule of three) — but don't pre-build primitives for milestones whose needs aren't yet visible.

The non-obvious "which primitive for what":

- **Harness identity → `HarnessIcon`** (brand artwork). Not a colored badge. The `HARNESS_COLOR` map (`harnessDisplay.ts`) supplies the accent hue for transcript attribution.
- **Run state → `StatusDot`** (or the `status-*` tokens directly). Pass `StatusDot`'s `label` only when the dot is the *sole* signal (it then gets an accessible name + tooltip); omit it when a sibling text label already carries the meaning.
- **Menus → `DropdownMenu` + `DropdownMenuItem`** (wraps `bits-ui`, so focus/keyboard/escape/ARIA come for free). Never hand-roll a menu.
- **`Badge`** is a plain neutral chip for incidental labels (e.g. an unknown transcript item kind) — it has no harness/status variants by design.
- Forms/overlays: `Button`, `Dialog`, `Input`, `Textarea`. Layout: `AppShell`, `SidebarPanel` + `SidebarSection`, `EmptyState`, `Banner` — express structure through these, not duplicated flex/border classes per screen.

## Theming

`theme.svelte.ts` owns the `.dark` class on `<html>`: `light`/`dark` pin a theme, `system` follows the OS and re-applies live on flips (only while on `system`). A pre-paint bootstrap in `index.html` prevents a startup flash for dark-mode users.

> **Deliberate divergence:** the theme preference persists in **`localStorage`** (`switchboard-theme`), per-device and non-synced, rather than the git-trackable `config.yaml` where other user-global state lives. Appearance is a device-local preference — syncing it across machines via a checked-in file would be wrong.
