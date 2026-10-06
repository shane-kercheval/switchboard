.PHONY: dev build open run install-app uninstall-app debug-app uninstall-debug-app deploy test test-browser lint fmt check check-rust check-frontend clean clean-stale install ios-crypto check-ios test-live test-live-claude test-live-codex test-live-antigravity

# Crates that carry live (`#[ignore]`-gated) harness tests.
LIVE_PKGS := -p switchboard-harness -p switchboard-dispatcher -p switchboard-app

# Fetch frontend (JS/TS) dependencies into node_modules. This installs the
# project's *dependencies*, not the app — to install the built app into
# /Applications, see `install-app` / `deploy`.
install:
	@node -e 'const m=Number(process.versions.node.split(".")[0]); if(m<22){console.error("Switchboard needs Node >= 22 (you have "+process.versions.node+"). Switch Node versions and retry.");process.exit(1)}'
	pnpm install --frozen-lockfile

# Incremental compilation is off for the whole-workspace targets below.
# It exists to speed up rebuilding one changed crate; for a target that builds
# everything it buys nothing and costs a great deal of disk (it grew to 26 GB
# here before the 2026-08-25 cleanup). `make dev` deliberately does NOT set
# this — the edit-rebuild loop is exactly the case incremental is for.
NO_INCR := CARGO_INCREMENTAL=0

# The desktop frontend is the `desktop/` package of the pnpm workspace rooted
# here; its scripts (and the Tauri CLI, which finds `desktop/src-tauri`) run
# from that directory. Formatting stays at the root, where it covers the root
# Markdown too.
DESKTOP := pnpm --dir desktop

DEFAULT_DEV_PORT := 1420
DEV_PORT ?= $(DEFAULT_DEV_PORT)

# Per-instance global config dir, keyed on DEV_PORT (the same value that keeps
# the Vite/Tauri dev servers from colliding). The default port resolves to the
# bare `switchboard-dev` — identical to the in-binary fallback a non-`make dev`
# launch (`cargo run`, IDE run button) uses — so alternating launch methods
# doesn't silently swap dev registries. Only additional instances on other
# ports get a `-<port>` suffix, so two simultaneous dev builds don't share one
# `workspace.yaml`. Read only by debug builds (see `workspace_config_path` in
# desktop/src-tauri/src/lib.rs). macOS path; v1 is macOS-only.
DEV_SUFFIX := $(if $(filter-out $(DEFAULT_DEV_PORT),$(DEV_PORT)),-$(DEV_PORT))
DEV_CONFIG_DIR := $(HOME)/Library/Application Support/switchboard-dev$(DEV_SUFFIX)

dev: install
	SWITCHBOARD_CONFIG_DIR="$(DEV_CONFIG_DIR)" VITE_DEV_PORT=$(DEV_PORT) VITE_GIT_BRANCH=$(shell git branch --show-current) $(DESKTOP) tauri dev --config '{"build":{"devUrl":"http://localhost:$(DEV_PORT)"}}'

# Release build of the macOS .app bundle (the only artifact that carries the
# bundled icon). `--bundles app` skips the .dmg packaging step. Output:
# target/release/bundle/macos/Switchboard.app
build: install
	$(DESKTOP) tauri build --bundles app
	# Guards a *runtime* precondition, not a distribution one: UNUserNotificationCenter
	# silently refuses to deliver from a bundle without a real signature, so losing
	# the ad-hoc signingIdentity would ship an app whose notifications just stop with
	# no error anywhere. Failing the build is the only place that regression is loud.
	codesign --verify --deep --strict target/release/bundle/macos/Switchboard.app

open:
	open target/release/bundle/macos/Switchboard.app

run: build open

# Install the *built* app into /Applications (distinct from `install`, which
# fetches dependencies). Copies an already-built bundle — run `build` first, or
# use `deploy` for build + install + launch in one step. Remove any prior bundle
# first so stale files from an older build don't linger inside the installed
# .app (a plain `cp` merges into the existing one).
install-app:
	rm -rf /Applications/Switchboard.app
	cp -R target/release/bundle/macos/Switchboard.app /Applications/

uninstall-app:
	rm -rf /Applications/Switchboard.app

# Debug build, installed and launched as a real app — the only way to exercise OS
# notifications. `UNUserNotificationCenter` refuses to deliver unless the process
# runs from a code-signed bundle installed under an Applications directory and
# registered with Launch Services; a bundle sitting in `target/` fails even when
# its signature is valid, and `make dev` (a bare binary) fails outright. Installs
# under a separate name so the real /Applications/Switchboard.app keeps its own
# notification authorization and is not replaced. Remove with `uninstall-debug-app`.
# Both are overridable on the command line. The identifier override is
# load-bearing, not cosmetic: macOS keys notification authorization on the bundle
# identifier, so without it the debug build would inherit — and could revoke —
# the installed app's permission.
#
# A *fresh* identifier is also the only reliable way to see the notification
# permission prompt again; macOS asks once per identifier and never re-asks. To
# capture the prompt for documentation, pair a throwaway identifier with the real
# display name so the dialog reads "Switchboard":
#
#   make debug-app DEBUG_APP_NAME=Switchboard DEBUG_APP_ID=com.switchboard.desktop.shot
#   make uninstall-debug-app DEBUG_APP_NAME=Switchboard
DEBUG_APP_NAME ?= Switchboard (debug)
DEBUG_APP_ID ?= com.switchboard.desktop.debug
DEBUG_APP := $(HOME)/Applications/$(DEBUG_APP_NAME).app
DEBUG_APP_CONFIG := {"productName":"$(DEBUG_APP_NAME)","identifier":"$(DEBUG_APP_ID)"}

debug-app: install
	$(DESKTOP) tauri build --debug --bundles app --config '$(DEBUG_APP_CONFIG)'
	codesign --verify --deep --strict "target/debug/bundle/macos/$(DEBUG_APP_NAME).app"
	rm -rf "$(DEBUG_APP)"
	mkdir -p "$(HOME)/Applications"
	cp -R "target/debug/bundle/macos/$(DEBUG_APP_NAME).app" "$(DEBUG_APP)"
	/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister -f "$(DEBUG_APP)"
	open "$(DEBUG_APP)"

uninstall-debug-app:
	rm -rf "$(DEBUG_APP)"

deploy: build install-app
	open /Applications/Switchboard.app

test:
	$(NO_INCR) cargo test --workspace --all-features --locked
	$(DESKTOP) test

# Real-WebKit frontend tests (Vitest browser mode) for the layout-coupled slice
# jsdom can't see. Kept out of `test` so the fast jsdom inner loop stays offline
# and quick. Ensures the WebKit binary first (idempotent, near-instant when
# already present) so this target — and `check`, which inlines it — is
# self-sufficient on a checkout that never ran `playwright install` by hand.
test-browser:
	$(DESKTOP) exec playwright install webkit
	$(DESKTOP) test:browser

lint:
	cargo fmt --all -- --check
	$(NO_INCR) cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
	$(DESKTOP) lint
	$(DESKTOP) check
	pnpm format:check

fmt:
	cargo fmt --all
	pnpm format

check-rust:
	cargo fmt --all -- --check
	$(NO_INCR) cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
	$(NO_INCR) cargo test --workspace --all-features --locked

check-frontend:
	pnpm install --frozen-lockfile
	$(DESKTOP) lint
	$(DESKTOP) check
	pnpm format:check
	$(DESKTOP) test
	$(DESKTOP) exec playwright install webkit
	$(DESKTOP) test:browser

check: check-rust check-frontend

# The iOS app (`ios/`) links `crates/remote-crypto` as an
# xcframework with UniFFI-generated Swift bindings. Both are build products,
# written into the package's gitignored `Generated/` directory; run
# `ios-crypto` once before opening the project in Xcode, and again after
# changing the crate.
IOS_PROJECT := ios/SwitchboardMobile.xcodeproj
IOS_KIT := ios/SwitchboardMobileKit
IOS_GENERATED := $(IOS_KIT)/Generated
IOS_STAGING := target/ios-crypto
IOS_DERIVED_DATA := target/ios-derived-data
IOS_CRYPTO_LIB := libswitchboard_remote_crypto.a
IOS_CRYPTO_FFI := switchboard_remote_cryptoFFI
IOS_RUST_TARGETS := aarch64-apple-ios aarch64-apple-ios-sim
UNIFFI_BINDGEN := tools/uniffi-bindgen/Cargo.toml

# The newest available iPhone simulator, unless overridden with
# `IOS_SIMULATOR_ID=<udid>`. Resolved lazily, so only `check-ios` pays for it.
IOS_SIMULATOR_ID ?= $(shell xcrun simctl list devices available | grep -E '^ +iPhone' | tail -1 | grep -oE '[0-9A-F]{8}(-[0-9A-F]{4}){3}-[0-9A-F]{12}')
IOS_DESTINATION = platform=iOS Simulator,id=$(IOS_SIMULATOR_ID)

# Everything is generated into the staging directory and moved into
# `Generated/` only once every output exists, so a failed run leaves the last
# working package in place.
ios-crypto:
	@lock_version() { awk '/^name = "uniffi"$$/ { getline; print $$3 }' "$$1"; }; \
	crate=$$(lock_version Cargo.lock); tool=$$(lock_version tools/uniffi-bindgen/Cargo.lock); \
	[ -n "$$crate" ] && [ "$$crate" = "$$tool" ] || { \
		echo "uniffi differs: crates/remote-crypto has $$crate, tools/uniffi-bindgen has $$tool. Pin the tool to the crate's version."; exit 1; }
	for target in $(IOS_RUST_TARGETS); do \
		$(NO_INCR) cargo rustc -p switchboard-remote-crypto --lib --profile ios --locked --target $$target --crate-type staticlib || exit 1; \
	done
	rm -rf $(IOS_STAGING)
	$(NO_INCR) cargo run -q --manifest-path $(UNIFFI_BINDGEN) --target-dir target --locked -- \
		generate --library target/aarch64-apple-ios/ios/$(IOS_CRYPTO_LIB) --language swift --out-dir $(IOS_STAGING)/bindings
	# Each xcframework's headers sit in a directory named for their module, so a
	# second Rust xcframework can never collide on `module.modulemap`.
	mkdir -p $(IOS_STAGING)/headers/$(IOS_CRYPTO_FFI) $(IOS_STAGING)/Generated/SwitchboardRemoteCrypto
	cp $(IOS_STAGING)/bindings/$(IOS_CRYPTO_FFI).h $(IOS_STAGING)/headers/$(IOS_CRYPTO_FFI)/
	cp $(IOS_STAGING)/bindings/$(IOS_CRYPTO_FFI).modulemap $(IOS_STAGING)/headers/$(IOS_CRYPTO_FFI)/module.modulemap
	cp $(IOS_STAGING)/bindings/switchboard_remote_crypto.swift $(IOS_STAGING)/Generated/SwitchboardRemoteCrypto/
	xcodebuild -create-xcframework \
		-library target/aarch64-apple-ios/ios/$(IOS_CRYPTO_LIB) -headers $(IOS_STAGING)/headers \
		-library target/aarch64-apple-ios-sim/ios/$(IOS_CRYPTO_LIB) -headers $(IOS_STAGING)/headers \
		-output $(IOS_STAGING)/Generated/SwitchboardRemoteCryptoFFI.xcframework
	test -s $(IOS_STAGING)/Generated/SwitchboardRemoteCrypto/switchboard_remote_crypto.swift
	test -f $(IOS_STAGING)/Generated/SwitchboardRemoteCryptoFFI.xcframework/Info.plist
	rm -rf $(IOS_GENERATED)
	mv $(IOS_STAGING)/Generated $(IOS_GENERATED)

# Checks that only Crypto/ imports the generated bindings and that a simulator
# exists before the slow Rust build, then builds the app and runs the package's
# tests on a simulator, builds Release, and checks both built Info.plists. A separate CI job, so `check` (and its wall
# time) is unchanged; `check` is therefore no longer everything CI runs.
check-ios:
	ios/scripts/check-binding-imports.sh
	@test -n "$(IOS_SIMULATOR_ID)" || { echo "No available iPhone simulator. Install one in Xcode, or pass IOS_SIMULATOR_ID=<udid>."; exit 1; }
	$(MAKE) ios-crypto
	xcodebuild test -quiet -project $(IOS_PROJECT) -scheme SwitchboardMobile -configuration Debug \
		-destination '$(IOS_DESTINATION)' -derivedDataPath $(IOS_DERIVED_DATA) CODE_SIGNING_ALLOWED=NO
	xcodebuild build -quiet -project $(IOS_PROJECT) -scheme SwitchboardMobile -configuration Release \
		-destination '$(IOS_DESTINATION)' -derivedDataPath $(IOS_DERIVED_DATA) CODE_SIGNING_ALLOWED=NO
	ios/scripts/check-info-plist.sh $(IOS_DERIVED_DATA)/Build/Products/Debug-iphonesimulator/SwitchboardMobile.app/Info.plist Debug
	ios/scripts/check-info-plist.sh $(IOS_DERIVED_DATA)/Build/Products/Release-iphonesimulator/SwitchboardMobile.app/Info.plist Release

test-live:
	cargo test --locked $(LIVE_PKGS) -- --ignored

# Per-harness live tests, to spend subscription quota on only the harness you
# care about (e.g. after a CLI version bump). Each filters by the harness name,
# which every live test for that harness carries (see the naming convention in
# AGENTS.md). Preview without spending quota by appending `--list` to the
# underlying cargo command.
test-live-claude:
	cargo test --locked $(LIVE_PKGS) claude -- --ignored

test-live-codex:
	cargo test --locked $(LIVE_PKGS) codex -- --ignored

test-live-antigravity:
	cargo test --locked $(LIVE_PKGS) antigravity -- --ignored

# Delete build artifacts nothing has touched in a week, WITHOUT throwing away
# the warm cache (unlike `clean`, which forces a full rebuild).
#
# Cargo never garbage-collects `target/` — it accumulates one set of artifacts
# per crate/feature/target-kind combination, forever. Left alone it reached
# 1,003,147 files / 249 GB here, at which point a no-op `cargo check` on the
# smallest crate took 7.2s and `make check` stopped finishing at all: the cost
# is a filesystem scan paid up front by every cargo invocation, before any
# compilation. Run this when builds start feeling slow; it is safe at any time
# (everything it deletes is regenerated from source).
clean-stale:
	@command -v cargo-sweep >/dev/null 2>&1 || { \
		echo "cargo-sweep not installed. Run: cargo install cargo-sweep"; exit 1; }
	@echo "target/ before: $$(du -sh target 2>/dev/null | cut -f1)"
	cargo sweep --time 7
	@echo "target/ after:  $$(du -sh target 2>/dev/null | cut -f1)"

clean:
	cargo clean
	rm -rf node_modules dist desktop/node_modules desktop/dist
