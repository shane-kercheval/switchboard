.PHONY: dev test lint fmt check clean install test-live

install:
	pnpm install --frozen-lockfile

dev:
	pnpm tauri dev

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
	cargo test --locked -p switchboard-harness -p switchboard-dispatcher -p switchboard-app -- --ignored

clean:
	cargo clean
	rm -rf node_modules dist
