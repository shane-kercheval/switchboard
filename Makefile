.PHONY: dev build open run install-app uninstall-app deploy test lint fmt check clean install test-live test-live-claude test-live-codex test-live-gemini test-live-antigravity

# Crates that carry live (`#[ignore]`-gated) harness tests.
LIVE_PKGS := -p switchboard-harness -p switchboard-dispatcher -p switchboard-app

# Fetch frontend (JS/TS) dependencies into node_modules. This installs the
# project's *dependencies*, not the app — to install the built app into
# /Applications, see `install-app` / `deploy`.
install:
	@node -e 'const m=Number(process.versions.node.split(".")[0]); if(m<22){console.error("Switchboard needs Node >= 22 (you have "+process.versions.node+"). Switch Node versions and retry.");process.exit(1)}'
	pnpm install --frozen-lockfile

DEFAULT_DEV_PORT := 1420
DEV_PORT ?= $(DEFAULT_DEV_PORT)

# Per-instance global config dir, keyed on DEV_PORT (the same value that keeps
# the Vite/Tauri dev servers from colliding). The default port resolves to the
# bare `switchboard-dev` — identical to the in-binary fallback a non-`make dev`
# launch (`cargo run`, IDE run button) uses — so alternating launch methods
# doesn't silently swap dev registries. Only additional instances on other
# ports get a `-<port>` suffix, so two simultaneous dev builds don't share one
# `workspace.yaml`. Read only by debug builds (see `workspace_config_path` in
# crates/app/src/lib.rs). macOS path; v1 is macOS-only.
DEV_SUFFIX := $(if $(filter-out $(DEFAULT_DEV_PORT),$(DEV_PORT)),-$(DEV_PORT))
DEV_CONFIG_DIR := $(HOME)/Library/Application Support/switchboard-dev$(DEV_SUFFIX)

dev:
	SWITCHBOARD_CONFIG_DIR="$(DEV_CONFIG_DIR)" VITE_DEV_PORT=$(DEV_PORT) VITE_GIT_BRANCH=$(shell git branch --show-current) pnpm tauri dev --config '{"build":{"devUrl":"http://localhost:$(DEV_PORT)"}}'

# Release build of the macOS .app bundle (the only artifact that carries the
# bundled icon). `--bundles app` skips the .dmg packaging step. Output:
# target/release/bundle/macos/Switchboard.app
build:
	pnpm tauri build --bundles app

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

deploy: build install-app
	open /Applications/Switchboard.app

test:
	cargo test --workspace --all-features --locked
	pnpm test

lint:
	cargo fmt --all -- --check
	cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
	pnpm lint
	pnpm check
	pnpm format:check

fmt:
	cargo fmt --all
	pnpm format

check:
	pnpm install --frozen-lockfile
	cargo fmt --all -- --check
	cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
	cargo test --workspace --all-features --locked
	pnpm lint
	pnpm check
	pnpm format:check
	pnpm test

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

test-live-gemini:
	cargo test --locked $(LIVE_PKGS) gemini -- --ignored

test-live-antigravity:
	cargo test --locked $(LIVE_PKGS) antigravity -- --ignored

clean:
	cargo clean
	rm -rf node_modules dist
