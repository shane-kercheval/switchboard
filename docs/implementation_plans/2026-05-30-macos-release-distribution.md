# macOS Release and Distribution

**Status:** proposed · **Created:** 2026-05-30

Ship a distributable Switchboard build that macOS users — developers already using Claude Code, Codex, or Gemini — can install with a single Homebrew command. The plan has three milestones: consolidate version management (eliminating a three-file sync problem), automate builds via a GitHub Release CI workflow, and create a Homebrew tap so `brew install --cask` works with zero Gatekeeper friction.

**Scope decisions:**

- **macOS only for v1** (stated project constraint). Tauri supports Linux and Windows, but CI and distribution are macOS-only here.
- **Homebrew custom tap, not direct `.dmg` download or Mac App Store.** macOS Gatekeeper blocks unsigned apps downloaded from the internet and, since Sequoia (15), removed the right-click > Open bypass for unsigned apps — users must navigate to System Settings to approve. Homebrew automatically removes the quarantine attribute on install, so users get zero Gatekeeper friction with no Apple Developer ID signing ($99/year). The target audience (developers running AI coding CLIs) already has Homebrew.
- **Universal binary (`universal-apple-darwin`).** One artifact covers both Apple Silicon and Intel Macs. Tauri supports this natively with `--target universal-apple-darwin`; the only cost is slightly longer CI build time.
- **No auto-update for v1.** Tauri ships an updater plugin; defer until the app has an established release cadence.
- **No Apple Developer ID signing for v1.** Not needed — Homebrew quarantine removal provides equivalent UX for the developer audience. Revisit if distribution via direct download or to non-developer users becomes a goal.

---

## Milestone 1 — Version management and local release build

### Goal & Outcome

Eliminate the three-file version sync problem and make a local `.dmg` build a single command.

- `crates/app/tauri.conf.json` no longer hardcodes its own version; it reads from `package.json` at build time.
- Bumping a release requires editing exactly two files: `package.json` and the workspace `Cargo.toml` — not three.
- `make release-build` produces a universal `.dmg` at a known, documented output path.
- CI uses the Rust toolchain pinned in `rust-toolchain.toml` directly, eliminating an unnecessary secondary install.
- A `RELEASING.md` at the repo root documents the end-to-end release checklist.

### Implementation Outline

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

For the Cargo workspace update, use whatever mechanism is cleanest and has no extra tooling dependency — `sed -i ''` on the `[workspace.package]` version line is fine on macOS (v1 is macOS-only, so portability is not a concern). The two values must match after the command runs; the DoD script verifies this.

**CI toolchain fix.** In `.github/workflows/hygiene.yml`, the action `uses: dtolnay/rust-toolchain@stable` installs the stable toolchain as a separate step, then `cargo` re-resolves `rust-toolchain.toml` (1.95.0) and installs it anyway — two toolchain installs for one build. Change to `uses: dtolnay/rust-toolchain@master` with no `toolchain:` input, which reads `rust-toolchain.toml` directly and installs only what the project pins. No behavior change for builds; eliminates the redundant install.

**`RELEASING.md`.** Create at repo root. Content: a numbered checklist covering the full release sequence through Homebrew tap update (M3 adds step 6). Initial content covers through M2:

1. `make bump-version VERSION=x.y.z`
2. Commit: `git commit -m "chore: release vx.y.z"`
3. Tag: `git tag -a vx.y.z -m "Release vx.y.z"`
4. Push tag: `git push origin vx.y.z` (triggers the release workflow)
5. Watch the GitHub Actions release run; verify the `.dmg` and `sha256.txt` are attached to the Release

Step 6 (Homebrew tap update) is added in M3.

### Definition of Done

- `make release-build` runs to completion and produces a `.dmg` under `target/universal-apple-darwin/release/bundle/dmg/`.
- `CFBundleShortVersionString` in `Switchboard.app/Contents/Info.plist` (inside the built artifact) matches `package.json`'s `version` field — proving the version link works.
- `make bump-version VERSION=9.9.9` updates both `package.json` and `Cargo.toml` to `9.9.9`; revert after verifying.
- `make check` green.
- No unit tests — version plumbing has no logic to test; the DoD verification above is the proof.

---

## Milestone 2 — GitHub Release CI workflow

### Goal & Outcome

Pushing a `v*` tag automatically builds the universal `.dmg` and publishes it as a GitHub Release. The SHA256 is included in the release assets to make the Homebrew tap update (M3) a copy-paste operation.

- A `release.yml` workflow triggers on `push: tags: v*`.
- The workflow builds `Switchboard_<version>_universal.dmg` on a macOS-15 runner.
- A GitHub Release is created automatically from the tag, with the `.dmg` and a `sha256.txt` attached as assets.

### Implementation Outline

**`.github/workflows/release.yml`.** Model the environment setup on `hygiene.yml` — same pnpm, Node, Rust toolchain (using the fixed `dtolnay/rust-toolchain@master` form from M1), and `Swatinem/rust-cache@v2`. The release-specific steps after env setup:

```yaml
- run: make install
- run: rustup target add x86_64-apple-darwin aarch64-apple-darwin
- run: pnpm tauri build --target universal-apple-darwin --bundles dmg
- name: Compute SHA256
  run: shasum -a 256 target/universal-apple-darwin/release/bundle/dmg/*.dmg | tee sha256.txt
- uses: softprops/action-gh-release@v2
  with:
    files: |
      target/universal-apple-darwin/release/bundle/dmg/*.dmg
      sha256.txt
```

`softprops/action-gh-release@v2` uses the `GITHUB_TOKEN` automatically available in Actions — no secrets configuration needed. It creates the Release from the pushed tag name and attaches the listed files.

**Timeout.** Set `timeout-minutes: 60`. A universal Tauri build compiles the Rust backend twice (two architectures) plus the Vite frontend; it is substantially slower than `make check`'s 30-minute ceiling.

**`RELEASING.md` update.** The human steps now end at `git push origin vx.y.z`. Note in the checklist that steps 4-5 are automated by the workflow.

### Definition of Done

- Push a pre-release tag (e.g., `v0.0.1-rc1`) → workflow runs to completion → GitHub Release is created with the `.dmg` and `sha256.txt` attached as assets.
- Manually download the `.dmg`, mount it, drag `Switchboard.app` to `/Applications/`, and confirm it opens (right-click > Open on first launch, or `xattr -dr com.apple.quarantine /Applications/Switchboard.app` — unsigned until Homebrew handles it in M3). This is the manual smoke-test; no automated test is appropriate.
- `CFBundleShortVersionString` in the installed app matches the release tag version.
- `make check` green.

---

## Milestone 3 — Homebrew tap and README

### Goal & Outcome

`brew install --cask shane-kercheval/switchboard/switchboard` installs Switchboard to `/Applications/` with zero Gatekeeper friction. The README reflects the real install path.

- A `homebrew-switchboard` GitHub repo exists under `shane-kercheval` with a valid `Casks/switchboard.rb`.
- The cask formula points to the current GitHub Release `.dmg` with the correct SHA256.
- This repo's README leads with the `brew install --cask` command as the primary install path.
- `RELEASING.md` includes the tap update step.

### Implementation Outline

**New repo: `homebrew-switchboard`.** Create at `github.com/shane-kercheval/homebrew-switchboard`. Homebrew's tap naming convention — `homebrew-<name>` — allows `brew tap shane-kercheval/switchboard` and `brew install --cask shane-kercheval/switchboard/switchboard`.

**Cask formula: `Casks/switchboard.rb`.** The SHA256 comes from the `sha256.txt` asset attached to the GitHub Release (M2). The URL pattern follows Tauri's artifact naming — verify the exact filename of the M2 release artifact before writing the formula, since the name may differ slightly from the pattern below:

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

- Apple Developer ID signing and notarization — Homebrew quarantine removal provides equivalent UX for the developer audience at zero cost.
- Auto-update (Tauri Updater plugin) — defer until a release cadence is established.
- Linux and Windows distribution — macOS only for v1; Tauri's multi-platform support can be leveraged later by adding platform runners to the release workflow.
- Mac App Store distribution — significant process overhead with no benefit for the target audience.
- Automated Homebrew tap updates — manual per-release update is appropriate for v1; revisit when release frequency warrants it.
- Submission to `homebrew/homebrew-cask` (the official tap) — requires a minimum install count threshold; a custom tap is the right starting point.
