# UI conventions

The durable rules for Switchboard's frontend — the things you can't infer by reading a single file. Per-component prop shapes live in the components' own doc-comments (`src/lib/components/ui/`) and the token list lives in `src/app.css`; this doc deliberately does not restate them, so it can't drift out of sync. The look-and-feel ("calm utility": restrained, dense, native-macOS-feeling, color carries meaning) is described in the UI-foundation plan.

## The one rule: name a semantic role, not a hue

A component that needs a color references a **semantic token**, never a raw palette value or a one-off Tailwind color (`text-amber-700`, `bg-blue-100`). Tokens are defined once in `src/app.css` as CSS custom properties, with light and dark as two mappings of the *same* names, exposed to Tailwind v4 via `@theme inline`. Because the utilities reference the variables by name (not by build-time value), dark mode is "just use the token" — a `.dark`-scoped override re-themes everything with no per-component work.

The token groups (see `app.css` for the exact names/values): **neutral surfaces** (`surface → panel → raised → border → muted → fg`, build depth by stepping the layers, not by adding shadows), **primary** (monochrome high-contrast action/selection), **accent** (a single restrained teal, reserved for focus rings/links/selected emphasis — not decoration), **destructive** / **warning**, and **status** (`status-{idle,processing,failed,cancelled}`, each a strong fg + a `-soft` bg).

Color carries meaning — never use a status hue decoratively, and never introduce a new hue without giving it a named role in `app.css`.

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
