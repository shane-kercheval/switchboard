# macOS Release and Distribution

**Status:** proposed · **Created:** 2026-05-30

Ship a distributable Switchboard build that macOS users — developers already using Claude Code, Codex, or Gemini — can install cleanly. The plan has five milestones: isolate dev and production config paths, consolidate version management (eliminating a three-file sync problem), automate builds via a GitHub Release CI workflow, add an in-app update notification, and create a Homebrew tap. The distributed build is **signed + notarized** with an Apple Developer ID so it launches without Gatekeeper friction.

**Distribution phasing.** Apple Developer ID enrollment can take days to weeks. The work splits accordingly:

- **Now (certificate-independent):** M1 (done), plus the **interim install path — build-from-source via `make install-app`**. Locally-built binaries are never quarantined, so `git clone && make install-app` produces a real `/Applications` app with zero Gatekeeper friction and silent updates (`git pull && make install-app`), no certificate required. This stays the permanent zero-cost / open-source fallback even after signing ships.
- **Deferred until the certificate is in hand:** the universal `.dmg` build (M2), the release CI + signing (M3), and the in-app update chip (M4) all target the download/Homebrew audience, which does not exist until the app can be signed. They are built when the certificate lands, so the signing and release pipeline can be exercised end-to-end rather than written blind. M5 (Homebrew tap) only makes sense for a signed app and is last.

**Scope decisions:**

- **macOS only for v1** (stated project constraint). Tauri supports Linux and Windows, but CI and distribution are macOS-only here.
- **Apple Developer ID signing + notarization (Individual enrollment, $99/yr).** This is the only path to a frictionless install of a *distributed* (downloaded or Homebrew-installed) macOS app. Contrary to a common belief, Homebrew does **not** strip quarantine — it *adds* `com.apple.quarantine` by default, and the `--no-quarantine` escape hatch is being removed (Homebrew stops supporting casks that fail Gatekeeper on 2026-09-01). So there is no free path to a frictionless prebuilt binary; signing is required. The cost is effectively per-release-year, not perpetual: an already-signed-and-notarized build keeps launching for new downloaders after membership lapses — only certificate *revocation* breaks it, and non-renewal does not trigger revocation.
- **Homebrew custom tap (of the signed app), not Mac App Store.** Given a signed app, Homebrew is the right channel: the target audience (developers running AI coding CLIs) already has it, and `brew install --cask` / `brew upgrade` is the cleanest update path. Mac App Store is ruled out for a hard technical reason, not just process overhead — see "Out of scope."
- **Universal binary (`universal-apple-darwin`).** One artifact covers both Apple Silicon and Intel Macs. Tauri supports this natively with `--target universal-apple-darwin`; the only cost is slightly longer CI build time.
- **Notification-only update check, not Tauri's built-in updater.** Tauri's updater plugin downloads and installs updates itself, bypassing Homebrew and leaving the package manager out of sync. Instead: check the GitHub Releases API on startup, compare to the running version, and show a chip if a newer release exists — clicking it shows the upgrade command. No in-app download, no Homebrew conflict.

---

## Milestone 1 — Dev/prod global config isolation

**Status: implemented** (landed alongside the multi-worktree dev support, since that change is what made the isolation load-bearing). Implemented form differs from the original outline below — see "As implemented."

### Goal & Outcome

Dev builds and the installed production app use separate global config paths, so a developer's `workspace.yaml` (the project/directory list) cannot bleed into or overwrite the production one and vice versa.

- `make dev` (a debug build) resolves global state to `~/Library/Application Support/switchboard-dev-<port>/`.
- The installed app (a release build) resolves global state to `~/Library/Application Support/switchboard/`.
- The separation is automatic — no env var to remember, no manual setup.

### As implemented

The outline below proposed a one-line build-profile switch on the `ProjectDirs::from` app name. The shipped version goes one step further to also isolate **two simultaneous dev instances** from each other (different worktrees running `make dev` on different ports), which the multi-worktree dev support makes a real scenario — without it, two dev builds share one `switchboard-dev/workspace.yaml` and last-writer-wins silently drops project-list edits.

- `crates/app/src/lib.rs` extracts a `workspace_config_path()` free function with two `#[cfg]` arms:
  - **Release** (`not(debug_assertions)`) — always `ProjectDirs::from("", "", "switchboard")`. No env var is consulted, so nothing can relocate the installed app's data.
  - **Debug** — honors a `SWITCHBOARD_CONFIG_DIR` path override if set; otherwise falls back to `ProjectDirs::from("", "", "switchboard-dev")`.
- `make dev` sets `SWITCHBOARD_CONFIG_DIR`, keyed on `DEV_PORT` (the same value that already keeps the Vite/Tauri dev servers from colliding). The **default** port resolves to the bare `~/Library/Application Support/switchboard-dev` — identical to the in-binary fallback above, so a `make dev` and a bare `cargo run`/IDE launch share one dev registry instead of silently diverging. Only **additional** instances on other ports get a `-<port>` suffix (e.g. `switchboard-dev-1421`), so two simultaneous dev builds get fully isolated global state.

`DEV_PORT` was chosen as the discriminator over the git branch because a branch name contains `/`, changes under `git checkout` mid-session, and isn't already a per-instance key — whereas the port is stable for the life of the `make dev` process and is already the uniqueness key for the dev servers. The debug arm of `workspace_config_path` is split into a pure `debug_workspace_config_path(override_dir)` helper so the override mapping is unit-tested without mutating process-global env (`std::env::set_var` is `unsafe` under edition 2024).

Per-project `.switchboard/` directories are unaffected — they're local to each working directory, not global, and don't need isolation.

### Original implementation outline (superseded by "As implemented")

The only change is in `crates/app/src/lib.rs` at the `ProjectDirs::from` call (line ~474), where `"switchboard"` is passed as the application name:

```rust
if let Some(dirs) = directories::ProjectDirs::from("", "", "switchboard") {
```

The `directories` crate derives the OS config path from this name (`~/Library/Application Support/<name>/` on macOS). Change it to select the name based on the build profile:

```rust
#[cfg(debug_assertions)]
let app_name = "switchboard-dev";
#[cfg(not(debug_assertions))]
let app_name = "switchboard";

if let Some(dirs) = directories::ProjectDirs::from("", "", app_name) {
```

`debug_assertions` is enabled for `cargo build` (debug) and `tauri dev`, and disabled for `cargo build --release` and `pnpm tauri build`. This maps exactly to the desired split: dev shell → `-dev` path, installed app → production path.

### Definition of Done

- Running `make dev`, opening the app, and adding a directory writes `workspace.yaml` to `~/Library/Application Support/switchboard-dev/`, not `switchboard/`. Running a second instance with `make dev DEV_PORT=1421` writes to `switchboard-dev-1421/` — the two do not share state.
- A release build (`make release-build`) reads from `~/Library/Application Support/switchboard/`.
- `make check` green.
- `debug_workspace_config_path` carries unit tests for the override and fallback branches.

---

## Milestone 2 — Local install + version management and release build

### Status

- **Build now (certificate-independent):** the build-from-source install flow. `build` / `open` / `run` / `install-app` / `deploy` already exist in the Makefile; this milestone adds `uninstall-app` and documents `make deploy` (build → install to `/Applications` → launch) as the interim install path users follow while Developer ID enrollment is pending.
- **Deferred until the certificate is in hand (lands with M3):** version consolidation, `make bump-version`, and the universal `.dmg` `make release-build` target. These are release-cutting machinery for the download/Homebrew audience and are pointless before the app can be signed.

### Goal & Outcome

Give build-from-source users a one-command install, eliminate the three-file version sync problem, and make a local `.dmg` build a single command.

- `make deploy` builds the app, installs `Switchboard.app` to `/Applications`, and launches it (`make install-app` copies an already-built bundle; `make uninstall-app` removes it). Build-from-source path — no signing, no quarantine.
- `crates/app/tauri.conf.json` no longer hardcodes its own version; it reads from `package.json` at build time.
- Bumping a release requires editing exactly two files: `package.json` and the workspace `Cargo.toml` — not three.
- `make release-build` produces a universal `.dmg` at a known, documented output path.
- CI uses the Rust toolchain pinned in `rust-toolchain.toml` directly, eliminating an unnecessary secondary install.
- A `RELEASING.md` at the repo root documents the end-to-end release checklist.

### Implementation Outline

**`make uninstall-app` target (build now).** The Makefile already carries `build`, `open`, `run`, `install-app` (`rm -rf` then `cp -R` into `/Applications`), and `deploy` (`build` → `install-app` → `open`). The interim install path is therefore just `make deploy`. This milestone adds the symmetric `uninstall-app`:

```makefile
uninstall-app:
	rm -rf /Applications/Switchboard.app
```

Note `install-app` deliberately does **not** build — it copies an already-built bundle (`target/release/bundle/macos/Switchboard.app`); `deploy` composes `build` + `install-app` + `open` for the one-command path. Because a locally built bundle carries no `com.apple.quarantine` attribute, it launches with no Gatekeeper prompt — first launch and every `git pull && make deploy` update alike. No `sudo` for admin users (the typical personal Mac); `/Applications` is admin-writable. (A `~/Applications` fallback for non-admin Macs can be documented in the README if it ever comes up; not worth a separate target for v1.)

**Version source consolidation.** Tauri 2.x supports a JSON file reference in the `"version"` field of `tauri.conf.json`: set it to a relative path and Tauri reads the `version` key from that file at build time. Change `"version": "0.0.1"` in `crates/app/tauri.conf.json` to `"version": "../../package.json"` (relative to `crates/app/`). `package.json` becomes the canonical version source for the app bundle. The workspace `Cargo.toml` `[workspace.package]` version is a separate Rust crate version that should stay in sync, but it is no longer a third independent source — only two files need bumping per release.

**`make release-build` target.** Add a `release-build` target to the Makefile that builds a universal `.dmg`. It must install both Rust targets before building, since even on an Apple Silicon runner the Intel cross-compile requires the `x86_64-apple-darwin` target:

```makefile
release-build:
    rustup target add x86_64-apple-darwin aarch64-apple-darwin
    pnpm tauri build --target universal-apple-darwin --bundles dmg
```

Output path: `target/universal-apple-darwin/release/bundle/dmg/Switchboard_<version>_universal.dmg`. Document this path in `RELEASING.md`.

**`make bump-version` target.** Add a `bump-version` target that updates both version files in one step, gated on a `VERSION` argument:

```makefile
bump-version:
    @if [ -z "$(VERSION)" ]; then echo "Usage: make bump-version VERSION=x.y.z"; exit 1; fi
    pnpm version $(VERSION) --no-git-tag-version
    # also update Cargo.toml workspace version to match
```

For the Cargo workspace update, the edit must be **anchored to the `[workspace.package]` stanza** so it can't accidentally rewrite a different `version =` line as the manifest grows. Cleanest is `cargo set-version --workspace $(VERSION)` (from `cargo-edit`), which scopes correctly with no hand-rolled regex — accept the dev-dependency on `cargo-edit`, or use a `sed` range restricted to the `[workspace.package]` block (not a bare global substitution). The target must then verify both files match (the DoD requires this); fold the check into the target so a wrong-line edit fails loudly rather than silently.

**CI toolchain fix.** In `.github/workflows/hygiene.yml`, the action `uses: dtolnay/rust-toolchain@stable` installs the stable toolchain as a separate step, then `cargo` re-resolves `rust-toolchain.toml` (1.95.0) and installs it anyway — two toolchain installs for one build. Change to `uses: dtolnay/rust-toolchain@master` with no `toolchain:` input, which reads `rust-toolchain.toml` directly and installs only what the project pins. No behavior change for builds; eliminates the redundant install.

**`RELEASING.md`.** Create at repo root. Content: a numbered checklist covering the full release sequence through Homebrew tap update (M5 adds step 6). Initial content covers through M2:

1. `make bump-version VERSION=x.y.z`
2. Commit: `git commit -m "chore: release vx.y.z"`
3. Tag: `git tag -a vx.y.z -m "Release vx.y.z"`
4. Push tag: `git push origin vx.y.z` (triggers the release workflow)
5. Watch the GitHub Actions release run; verify the `.dmg` and `sha256.txt` are attached to the Release

Step 6 (Homebrew tap update) is added in M5.

### Definition of Done

**Build now:**

- `make deploy` builds, installs `Switchboard.app` to `/Applications`, and launches it with **no Gatekeeper prompt** (locally built → unquarantined). `make uninstall-app` removes it.
- `make check` green.

**Deferred (verify when the release machinery lands with M3):**

- `make release-build` runs to completion and produces a `.dmg` under `target/universal-apple-darwin/release/bundle/dmg/`.
- `CFBundleShortVersionString` in `Switchboard.app/Contents/Info.plist` (inside the built artifact) matches `package.json`'s `version` field — proving the version link works.
- `make bump-version VERSION=9.9.9` updates both `package.json` and `Cargo.toml` to `9.9.9`; revert after verifying.
- No unit tests — version plumbing has no logic to test; the DoD verification above is the proof.

---

## Milestone 3 — GitHub Release CI workflow (signed)

**Status: deferred until the Apple Developer ID certificate + notarization credentials are in hand.** This is where the certificate first becomes load-bearing; build it then so the signing/notarization steps can be run for real, not written blind.

### Goal & Outcome

Pushing a `v*` tag automatically builds the universal `.dmg`, **signs and notarizes it**, and publishes it as a GitHub Release. The SHA256 is included in the release assets to make the Homebrew tap update (M5) a copy-paste operation.

- A `release.yml` workflow triggers on `push: tags: v*`.
- The workflow builds, signs, and notarizes `Switchboard_<version>_universal.dmg` on a macOS-15 runner.
- A GitHub Release is created automatically from the tag, with the `.dmg` and a `sha256.txt` attached as assets.
- Pre-release tags (anything with a `-` suffix, e.g. `v0.0.1-rc1`) are published as GitHub **pre-releases** so they are excluded from `releases/latest` — otherwise an RC test tag would advertise itself to every user's update chip and to Homebrew.

### Implementation Outline

**`.github/workflows/release.yml`.** Model the environment setup on `hygiene.yml` — same pnpm, Node, Rust toolchain (using the fixed `dtolnay/rust-toolchain@master` form from M1), and `Swatinem/rust-cache@v2`. The release-specific steps after env setup:

```yaml
permissions:
  contents: write   # action-gh-release needs write to create the Release; org/repo token defaults may be read-only
...
- run: make install
- run: rustup target add x86_64-apple-darwin aarch64-apple-darwin
- name: Build, sign, and notarize
  env:
    APPLE_CERTIFICATE: ${{ secrets.APPLE_CERTIFICATE }}              # base64 .p12 (Developer ID Application)
    APPLE_CERTIFICATE_PASSWORD: ${{ secrets.APPLE_CERTIFICATE_PASSWORD }}
    APPLE_SIGNING_IDENTITY: ${{ secrets.APPLE_SIGNING_IDENTITY }}     # "Developer ID Application: <Name> (TEAMID)"
    APPLE_ID: ${{ secrets.APPLE_ID }}
    APPLE_PASSWORD: ${{ secrets.APPLE_PASSWORD }}                     # app-specific password for notarization
    APPLE_TEAM_ID: ${{ secrets.APPLE_TEAM_ID }}
  run: pnpm tauri build --target universal-apple-darwin --bundles dmg
- name: Compute SHA256
  run: shasum -a 256 target/universal-apple-darwin/release/bundle/dmg/*.dmg | tee sha256.txt
- uses: softprops/action-gh-release@v2
  with:
    prerelease: ${{ contains(github.ref_name, '-') }}
    files: |
      target/universal-apple-darwin/release/bundle/dmg/*.dmg
      sha256.txt
```

**Signing + notarization.** Tauri performs both natively when the `APPLE_*` env vars are present — it signs the bundle with the Developer ID identity and submits to Apple's notary service, stapling the ticket. The six secrets above are configured once in the repo settings after enrollment clears (the `.p12` is exported from the signing keychain and base64-encoded). With the secrets absent, the same workflow still builds an *unsigned* `.dmg`, so the pipeline is testable before the certificate arrives — but the signed DoD below requires the secrets.

`softprops/action-gh-release@v2` uses the `GITHUB_TOKEN` automatically available in Actions (paired with the `permissions: contents: write` above). It creates the Release from the pushed tag name and attaches the listed files.

**Timeout.** Set `timeout-minutes: 60`. A universal Tauri build compiles the Rust backend twice (two architectures) plus the Vite frontend, then notarization waits on Apple's service; it is substantially slower than `make check`'s 30-minute ceiling.

**Test gate.** Tags are cut only from a `main` commit already green on `hygiene.yml` — state this assumption in `RELEASING.md` rather than re-running the full `make check` inside the (already slow) release job.

**`RELEASING.md` update.** The human steps now end at `git push origin vx.y.z`. Note in the checklist that steps 4-5 are automated by the workflow.

### Definition of Done

- Push a pre-release tag (e.g., `v0.0.1-rc1`) → workflow runs to completion → GitHub Release is created with the `.dmg` and `sha256.txt` attached, and is marked **pre-release** (verify it does not appear as `releases/latest`).
- Manually download the `.dmg`, mount it, drag `Switchboard.app` to `/Applications/`, and confirm it opens **with no Gatekeeper dialog** — proving signing + notarization worked. (`spctl -a -vv /Applications/Switchboard.app` should report `accepted` / `source=Notarized Developer ID`.) This is the manual smoke-test; no automated test is appropriate.
- `CFBundleShortVersionString` in the installed app matches the release tag version.
- `make check` green.

---

## Milestone 4 — In-app update notification

**Status: deferred until releases exist (with M3).** The chip polls the GitHub Releases API, which has nothing to read until M3 publishes releases, and its audience is the download/Homebrew user — not the interim build-from-source user, who updates via `git pull`. Build it alongside M3 so it can be exercised end-to-end. (It's certificate-independent, but pointless before there's a release to detect.)

### Goal & Outcome

When a newer GitHub Release exists, a chip appears in the app header telling the user how to upgrade. No in-app download, no Homebrew conflict.

- On every launch, the app checks the GitHub Releases API for the latest version.
- If the latest release is newer than the running version, a chip is shown in the header (e.g. *"v0.2.0 available"*).
- Clicking the chip opens a small flyout showing the upgrade command to run, with a copy button.
- A failed check (no network, GitHub down, rate limit hit) is silent — the chip simply doesn't appear and the app launches normally.

**Upgrade command is a single source-of-truth constant**, so the chip stays correct across the distribution phasing: it defaults to the build-from-source command (`git pull && make install-app`) and flips to `brew upgrade --cask switchboard` once the signed Homebrew tap (M5) is live. One constant to change, one place.

### Implementation Outline

**New Tauri command: `check_for_update`.** Follows the existing two-layer pattern: a thin `#[tauri::command]` shim in `lib.rs` and a free `*_impl` in `commands.rs`. The `*_impl` functions take only plain Rust types (never `AppHandle`/`State`), so the **shim extracts the running version** (`app.package_info().version.to_string()`) and passes it in:

```rust
pub async fn check_for_update_impl(current_version: &str) -> Option<String>
```

Returns `Some(version)` when a newer release is available, `None` when up to date or when the check fails for any reason. All failure modes (network error, parse error, timeout, non-200 response) return `None` via `?` — no error surface to the frontend. The command is registered in `lib.rs` alongside the others.

It hits `https://api.github.com/repos/shane-kercheval/switchboard/releases/latest`, parses `tag_name` (e.g. `"v0.2.0"`), and compares to `current_version`. **Factor the comparison into a pure helper** — `fn is_newer(current: &str, latest_tag: &str) -> bool` (strips a leading `v`, parses both with `semver`, returns `false` on any parse failure) — so it is unit-testable without the network. Use **semver ordering, not string equality**, so a rollback tag doesn't surface as "update available".

Dependencies: `cargo add reqwest -p switchboard-app --features json,rustls-tls` (no OpenSSL, macOS-first) and `cargo add semver -p switchboard-app` — `semver` is only transitive in `Cargo.lock` today, so direct use needs a direct dep (do not hand-edit `Cargo.toml`; see AGENTS.md). Set a 5-second client timeout. GitHub's API requires a `User-Agent` header; use `"switchboard-app"`. The request comes from Rust, not the webview, so no CSP change is needed.

**Frontend: update chip.** Call `invoke('check_for_update')` in the app's top-level `onMount`. If it returns a version string, render a small chip in the title bar — model its placement on the existing `DevIndicator` (the precedent for a build-gated chip pinned in the `App.svelte` title bar). There is **no `ui/Popover` primitive** (the original plan assumed one); **reuse the existing "resume in terminal" command-display UI** (the component that shows a shell command with a copy button — around `AgentActionsMenu`/`Sidebar`; confirm the exact component when building). The flyout shows:

```
v{version} is available.
{upgradeCommand}      ← the single source-of-truth constant; multiline allowed
```

The command may be multiline (`git pull && make install-app`) — render it accordingly **without changing the existing dialog's formatting**. Reuse the existing copy-to-clipboard affordance (the clipboard plugin is already wired). The chip and flyout are absent when `check_for_update` returns null.

**Privacy note.** Add one line to `README.md` under a new "Privacy" section or inline in the Installation section: on launch, Switchboard checks GitHub's releases API to detect updates; this request is visible to GitHub as any other API call. One sentence is enough — the audience understands what an API call is.

### Definition of Done

- Unit tests on the pure `is_newer` helper: `v0.2.0` > `0.1.9`; equal versions not newer; prerelease ordering; malformed / missing `tag_name` → not newer (`false`); leading-`v` strip.
- A component-level test (mock `invoke`) confirms the chip renders when `check_for_update` resolves to a version and is absent when it resolves to null.
- In a debug build pointing at a mock or real GitHub release with a higher version, the chip appears in the title bar.
- With no network, the app launches normally with no chip and no error.
- The copy button copies the current upgrade command (the source-of-truth constant) to the clipboard.
- The chip is absent when already on the latest version.
- `make check` green.

---

## Milestone 5 — Homebrew tap and README

**Status: deferred, and last.** A Homebrew cask only delivers a frictionless install for a *signed + notarized* app (Homebrew adds quarantine; it does not remove it). So this lands after M3 is producing signed releases. Until then the README leads with the build-from-source install.

### Goal & Outcome

`brew install --cask shane-kercheval/switchboard/switchboard` installs Switchboard to `/Applications/` with zero Gatekeeper friction — because the app is signed + notarized (M3), not because of anything Homebrew does to quarantine. The README reflects the real install path.

- A `homebrew-switchboard` GitHub repo exists under `shane-kercheval` with a valid `Casks/switchboard.rb`.
- The cask formula points to the current GitHub Release `.dmg` with the correct SHA256.
- This repo's README leads with the `brew install --cask` command as the primary install path.
- `RELEASING.md` includes the tap update step.

### Implementation Outline

**New repo: `homebrew-switchboard`.** Create at `github.com/shane-kercheval/homebrew-switchboard`. Homebrew's tap naming convention — `homebrew-<name>` — allows `brew tap shane-kercheval/switchboard` and `brew install --cask shane-kercheval/switchboard/switchboard`.

**Cask formula: `Casks/switchboard.rb`.** The SHA256 comes from the `sha256.txt` asset attached to the GitHub Release (M3). The URL pattern follows Tauri's artifact naming — verify the exact filename of the M3 release artifact before writing the formula, since the name may differ slightly from the pattern below:

```ruby
cask "switchboard" do
  version "0.1.0"
  sha256 "<sha256-from-release-sha256.txt>"

  url "https://github.com/shane-kercheval/switchboard/releases/download/v#{version}/Switchboard_#{version}_universal.dmg"

  name "Switchboard"
  desc "Human-directed orchestrator for AI coding agents"
  homepage "https://github.com/shane-kercheval/switchboard"

  app "Switchboard.app"
end
```

**Per-release tap update process.** For each new release: download `sha256.txt` from the GitHub Release page, update `version` and `sha256` in `Casks/switchboard.rb`, commit and push to `homebrew-switchboard`. Add as step 6 in `RELEASING.md`:

> 6. Download `sha256.txt` from the GitHub Release. Update `version` and `sha256` in `homebrew-switchboard/Casks/switchboard.rb`. Commit and push to `homebrew-switchboard`.

**README update.** Replace the Status section's "not yet usable software" note. Add an Installation section above Local development:

```markdown
## Installation

Requires macOS. Install via Homebrew:

```sh
brew install --cask shane-kercheval/switchboard/switchboard
```

After installation, Switchboard appears in `/Applications/` and Launchpad.

To update to a new release:

```sh
brew upgrade --cask switchboard
```
```

Update the Status section to reflect that the app is now installable (remove "not yet usable software"; keep the GitHub link and high-level description).

### Definition of Done

- `brew install --cask shane-kercheval/switchboard/switchboard` completes without error (test with `brew uninstall switchboard` first if already installed, to simulate a fresh install).
- Switchboard.app opens from `/Applications/` with no Gatekeeper dialog or security warning.
- `brew info --cask shane-kercheval/switchboard/switchboard` shows the correct version and source URL.
- README install instructions render correctly on GitHub and link to a working tap.
- `make check` green.

---

## Out of scope (explicitly)

- Auto-update (Tauri Updater plugin) — defer until a release cadence is established. (Were Switchboard ever distributed *without* Homebrew, the updater would be the way to avoid per-update Gatekeeper friction on an unsigned build — noted for the record, not v1.)
- Linux and Windows distribution — macOS only for v1; Tauri's multi-platform support can be leveraged later by adding platform runners to the release workflow.
- Mac App Store distribution — **architecturally incompatible, not merely overhead.** Every App Store app must run in the App Sandbox, which forbids spawning arbitrary external CLIs (`claude`/`codex`/`gemini`) and reading files outside the container (`~/.claude/…`, `~/.codex/…`, arbitrary project dirs) — exactly what Switchboard does. Mandatory per-release App Review is also incompatible with a frequent-update cadence. Developer ID distribution (this plan) is the only viable signed path.
- Automated Homebrew tap updates — manual per-release update is appropriate for v1; revisit when release frequency warrants it.
- Submission to `homebrew/homebrew-cask` (the official tap) — requires a minimum install count threshold; a custom tap is the right starting point.
