# App icon

## Canonical source

`docs/images/single-agent-5.png` — 1024×1024 RGBA, transparent background, squircle
shape baked in. This is the master the app icons are generated from. Regenerate from
this file (or its replacement); don't hand-edit the files in `crates/app/icons/`.

Earlier iterations (`single-agent-3.png`, `single-agent-4.png`) were rejected:
`-3` was a photorealistic scene (mesh/screws/cable + a baked drop shadow) that turned to
mush at small sizes; `-4` was clean but dropped the switchboard identity entirely. `-5`
keeps the identity (robot-in-disc, brass plug + purple cable, panel) in a bold,
simplified style that survives downscaling.

## How the icons are generated

`pnpm tauri icon docs/images/single-agent-5.png -o crates/app/icons` produces every size
and format the bundle references (`tauri.conf.json` → `bundle.icon`): `32x32.png`,
`128x128.png`, `128x128@2x.png`, `icon.icns`, `icon.ico`, plus the Windows `Square*Logo`
set. Tauri also emits `ios/`, `android/`, and `64x64.png` — delete those; this is a
macOS app and they aren't referenced.

Tauri's built-in downscaling is soft. The committed icons were instead rendered per-size
with Lanczos + an unsharp mask (stronger sharpening at ≤64px), and `icon.icns` was rebuilt
with `iconutil -c icns` from a sharpened `.iconset` so the size macOS actually shows in the
dock/CMD+Tab is crisp. If you regenerate, reapply that sharpening pass rather than shipping
Tauri's raw output.

## Why all sizes are kept (including the mushy 16px)

`icon.icns` is a single container holding every size (16→1024). macOS picks the size that
matches the context — 16px in Finder list/column views, 32px in smaller contexts, 128px+ in
the dock and app switcher. Omitting a size does **not** fall back to your nicely-downscaled
large art; macOS grabs the nearest size and downscales it itself with a worse filter. So
every slot is kept on purpose.

32px and up look good. **16px is mushy** — a panel + robot + cable is too much for 16×16,
and that's inherent to detailed art, not a pipeline bug. The real fix (if 16px ever matters
enough) is a hand-simplified mini-variant — e.g. just the robot face, no panel/cable —
dropped into only the 16px (and maybe 32px) `.iconset` slots before building the `.icns`.
Not worth it yet; noting it here so the reminder is durable instead of living in degraded
files.

## Seeing the icon after a change

The icon only lives in the bundled `.app`, not the `make dev` debug binary (dev shows a
generic icon — expected). Build the bundle and launch it:

```
make build
open target/release/bundle/macos/Switchboard.app
```

macOS caches icons aggressively. If a stale icon persists, re-register the bundle and
restart the UI services:

```
/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister -f target/release/bundle/macos/Switchboard.app
killall Dock Finder
```
