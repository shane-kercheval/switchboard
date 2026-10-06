# Switchboard desktop app

The macOS app: a Svelte 5 + Vite + TypeScript + Tailwind v4 frontend (`src/`, `tests/`) in a Tauri 2 shell (`src-tauri/`, the `switchboard-app` crate). The shell drives the shared Rust crates in [`../crates/`](../crates/) — agent CLI adapters, the dispatcher, persistence.

The build prerequisites are the same as the root README's [Install](../README.md#install) section — Xcode Command Line Tools, Rust (rustup), Node (pinned in [`.nvmrc`](../.nvmrc)), and pnpm (the version in the root `package.json`'s `packageManager`). If you've installed the app, you already have everything.

## Commands

Run everything from the **repo root**, through `make`, not from this directory:

```sh
make install      # one-time: pnpm install --frozen-lockfile (the whole pnpm workspace)
make dev          # run the Tauri dev shell
make test         # all Rust + frontend tests (fast, offline jsdom suite)
make test-browser # real-WebKit frontend suite (Vitest browser mode); installs WebKit if needed
make lint         # clippy, eslint, svelte-check, prettier
make check        # the desktop gate (incl. the browser suite) — run before opening a PR
make test-live    # live-harness suite against the real agent CLIs (developer-local)
make deploy       # build, install to /Applications, and launch
```

`make test-live` exercises the adapters against the real `claude` / `codex` / `antigravity` CLIs to catch upstream drift. See [`crates/harness/tests/README.md`](../crates/harness/tests/README.md) for what it covers and how to set it up.

`make test-browser` (and `make check`) run the frontend suite in a real WebKit engine via Vitest browser mode. The target installs a Playwright-managed WebKit build on demand — the first run downloads ~100 MB (cached afterward), so it needs network access once; no extra system packages are required on macOS. The default `make test` stays jsdom-only and needs none of this.

## Developing without an agent CLI installed

If no agent CLI is on your `PATH` (or you don't want to burn quota during UI iteration), launch with the mock harness:

```sh
SWITCHBOARD_HARNESS=mock make dev
```

The mock emits canned streaming responses (`Mock response to: <prompt> — replied by mock harness.`) — identical event-stream shape to a real harness, so the UI exercises every code path, and the startup binary-not-found banner stays hidden.

## Where to read next

- [`AGENTS.md`](../AGENTS.md) — project orientation and conventions.
- [`docs/ui-conventions.md`](../docs/ui-conventions.md) — the token model, the `src/lib/components/ui/` primitives, and theming.
- [`docs/implementation_plans/`](../docs/implementation_plans/) — the roadmap and per-phase plans.
