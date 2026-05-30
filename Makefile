.PHONY: dev build test lint fmt check clean install test-live test-live-claude test-live-codex test-live-gemini test-live-antigravity

# Crates that carry live (`#[ignore]`-gated) harness tests.
LIVE_PKGS := -p switchboard-harness -p switchboard-dispatcher -p switchboard-app

install:
	pnpm install --frozen-lockfile

dev:
	pnpm tauri dev

# Release build of the macOS .app bundle (the only artifact that carries the
# bundled icon). `--bundles app` skips the .dmg packaging step. Output:
# target/release/bundle/macos/Switchboard.app
build:
	pnpm tauri build --bundles app

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
