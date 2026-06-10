# Frontend WebKit component tests (real-layout coverage)

## Background & motivation

The frontend unit suite runs under **jsdom**, which has **no layout engine**: `scrollHeight`/`clientHeight`/`getBoundingClientRect` return zeros, CSS (`max-height`, `mask-image`, `container-type`/`cqh`) is parsed but never *applied*, and `ResizeObserver` is a no-op polyfill (`tests/setup.ts`). The compact-transcript feature has a substantial slice of behavior that lives entirely in that blind spot — overflow detection driving the per-message toggle, the live-cap sizing, the scroll re-anchoring on collapse/expand/stream-completion. Two real bugs (the user-message re-collapse toggle vanishing, and scrollbar/keyboard scroll-up being unable to escape auto-follow) shipped green precisely because jsdom can't see them; they were caught only by a hand-built WebKit walkthrough.

We are about to keep iterating on this layout-sensitive UI (and the upcoming transcript-virtualization milestone is even more layout-coupled). So we want **automated** coverage of the real-layout behaviors, run in **WebKit** because Tauri's webview is WKWebView and the gaps we're guarding (no native CSS scroll-anchoring, container-query/mask behavior) are WebKit-specific.

This was de-risked already: a throwaway Vite + Playwright-WebKit harness mounted the real `UnifiedTranscript`, applied real Tailwind CSS, and reproduced all the behaviors (clip = real 224px, scroll-hold, streaming pin, indicator) with the two fixed bugs confirmed fixed. This plan turns that throwaway into a permanent, idiomatic test layer.

**Scope discipline.** This is *additive coverage for the layout-coupled slice only*. The jsdom suite stays as the fast inner loop and keeps covering logic; we do **not** port it wholesale to the browser, and we do **not** build a general "everything runs in a browser" framework. Add browser tests only for behavior jsdom genuinely cannot exercise.

## Chosen approach (and what it's chosen over)

- **Vitest browser mode (`@vitest/browser`) with the Playwright provider, browser = WebKit** — *chosen over* a standalone Playwright script/harness. Browser mode reuses the project's existing Vite + Tailwind pipeline (so the real CSS "just works" with no separate content-scanning config — the throwaway harness needed a manual `@source` fix precisely because it bypassed the project config), uses one test runner, and lets tests be authored in the same component-mount + `vi.mock` + state-seeding style the existing `UnifiedTranscript.test.ts` already uses. A standalone Playwright system would be a parallel test stack with its own glue and a different authoring style — more to maintain for no benefit here.
- **WebKit specifically** — *chosen over* Chromium. WKWebView is what ships in the Tauri app; the behaviors under test (scroll anchoring absence, `container-type`/`cqh`, mask gradients) are where engines differ. Chromium could be added later if cross-engine drift becomes a concern, but it is out of scope now.
- **Run in CI, as a distinct target** — *chosen over* developer-local-only (the policy the Rust *live* tests use). Live tests are local-only because they need rotating subscription auth and burn quota (see AGENTS.md "Live testing"). Browser tests have **none** of that: they're fully hermetic (a downloadable, free browser binary; deterministic; no network/auth). CI already runs on `macos-15`, where the Playwright WebKit build runs headless. So they belong in CI — but as their own step/target so the fast jsdom suite stays the quick inner loop.
  - **This is the one decision worth an explicit confirm:** it adds a browser download + a few seconds to CI. If the CI time budget is a hard constraint, the fallback is developer-local + a scheduled CI job, mirroring the live-test split. Recommendation is CI-included; flag for sign-off in review.

## Required reading (before implementing)

The implementing agent must read these before writing code — browser mode and the Svelte browser-render API differ from the jsdom `@testing-library/svelte` + `screen` style the repo uses today:

- Vitest browser mode guide: https://vitest.dev/guide/browser/
- Browser config / providers (Playwright provider, `browser.instances`, headless): https://vitest.dev/guide/browser/config.html and https://vitest.dev/guide/browser/playwright.html
- Test "projects" (running jsdom and browser suites from one config): https://vitest.dev/guide/projects.html
- `vitest-browser-svelte` (the render-in-browser API; the browser-mode counterpart to `@testing-library/svelte`): https://github.com/vitest-dev/vitest-browser-svelte
- Interaction/locator API in browser mode (`page`, locators, `userEvent`) from `@vitest/browser/context`: https://vitest.dev/guide/browser/locators.html and https://vitest.dev/guide/browser/interactivity-api.html
- Playwright WebKit (engine the provider drives): https://playwright.dev/docs/browsers#webkit

Also re-read, in-repo: the **`test` block inside `vite.config.ts`** (there is **no** `vitest.config.ts` — the Vitest config, including `environment`, `setupFiles`, `include`, the 15s `testTimeout`, and the `VITEST`-gated `resolve.conditions: ["browser"]`, lives there), `tests/setup.ts`, `src/lib/components/UnifiedTranscript.test.ts` (the seeding/`vi.mock` patterns to carry over), and `src/lib/components/UnifiedTranscript.svelte` (the behaviors under test). Also `Makefile` (`check:`, `test:`, `install:`) and `.github/workflows/hygiene.yml` (the single `make check` job, `macos-15`, `timeout-minutes: 30`). The throwaway harness facts captured here are ground truth for the mock surface and sizing (see Milestone 1).

## Milestone 1 — Browser-test harness and conventions

### Goal & outcome

Stand up Vitest browser mode (WebKit) alongside the existing jsdom suite, with a reusable way to mount a real Svelte component in real WebKit and seed transcript state, plus one smoke test proving real layout actually applies. This milestone establishes the patterns every later browser test reuses.

Functional outcomes:

- A developer can write a test that mounts `UnifiedTranscript` (or another component) in a real WebKit page where CSS, layout geometry, and `ResizeObserver` are real.
- That test can seed agent transcripts/runtimes and drive the component the same conceptual way the jsdom tests do (mock the Tauri/native modules; mutate the exported `$state` stores), and can assert real measured geometry (e.g. a clipped element's `clientHeight` < its `scrollHeight`).
- The browser suite runs via its own command and is part of the CI gate, while `make test` / `pnpm test` stay jsdom-only and fast.
- The repo's conventions doc records what a "browser test" is and the run policy, so the next contributor (and the virtualization milestone) reuses this rather than reinventing it.

### Implementation outline

1. **Stand up the stack and fail fast (compatibility gate — do this first).** Add `@vitest/browser`, `playwright`, and `vitest-browser-svelte` via the package manager (per AGENTS.md, use the CLI, never hand-edit versions), install the WebKit binary (`playwright install webkit`), wire a minimal browser project (Playwright provider, `webkit`, headless), and get a trivial "mount *something* in a real browser and assert it rendered" test passing — **before** building the reusable helper or conventions below. Rationale: the whole "author in the same style" promise rests on `@vitest/browser` matching Vitest 4 and `vitest-browser-svelte` supporting Svelte 5 + Vitest 4. If that doesn't align, the mount API changes; discover it on step 1, not after building conventions on top of it.
2. **Two-project config, partitioned correctly.** The Vitest config is the `test` block in **`vite.config.ts`** (there is no `vitest.config.ts`). Convert it to `test.projects`: a **jsdom** project that carries the current settings (`environment: "jsdom"`, `setupFiles: tests/setup.ts`, the 15s `testTimeout`) with its `include` **narrowed** so it no longer matches the browser specs, plus a **browser** project that includes only those. Pick a clear boundary (a filename suffix or a directory) so no file double-runs, and state it in the conventions doc so future specs land in the right project. Two specifics that are load-bearing:
   - The `VITEST`-gated `resolve.conditions: ["browser"]` (which exists to force Svelte's *client* build under Node/jsdom) must live on the **jsdom project only** — the browser project resolves browser conditions natively, so inheriting the injection there is redundant and a needless resolution-ambiguity risk.
   - `tests/setup.ts`'s polyfills (`ResizeObserver`, `matchMedia`, `scrollIntoView`) must stay bound to the jsdom project so they do **not** leak into the browser project, where those are real.
3. **Keep `pnpm test` jsdom-only.** A bare `vitest run` against a projects config runs **every** project. So pin `package.json` `"test": "vitest run --project jsdom"` and add a separate `"test:browser"` (browser project). Without this, `make test` — the fast inner loop the whole plan is built to preserve — silently starts paying the browser cost. Narrowing the `include` globs (step 2) is necessary but not sufficient on its own; the `--project` pin is what guarantees it.
4. **Real CSS must be applied.** The browser project needs the app's stylesheet loaded so Tailwind utilities (`max-h-[14rem]`, the mask gradients, `container-type`) actually exist in the page — the whole point. Because browser mode uses the project's Vite + Tailwind plugin, importing `src/app.css` from a browser-project setup file is expected to be sufficient (no manual Tailwind `@source` needed, unlike the bypassed-config throwaway). Verify by asserting a *computed* style (step 7), not a class string.
5. **Reusable mount + seed helper.** Provide one small helper (the pattern, not a framework) that: mounts a component into a **sized** container (the transcript is `flex-1`; give the mount target a definite height, e.g. a fixed-height parent, so overflow/scroll are meaningful), mocks the three modules the component transitively needs, and exposes the seeding surface. The mock surface — established empirically and load-bearing — is exactly:
   - `@tauri-apps/api/core` → `invoke` (async no-op), `convertFileSrc` (identity)
   - `@tauri-apps/api/event` → `listen` (async, returns a no-op unlisten)
   - `$lib/native` → `copyText` (async no-op)
   Seeding is direct mutation of the exported `transcripts` / `runtimes` `$state` from `$lib/state/index.svelte` (same as the jsdom tests), plus `transcriptPreview` helpers for compact state. `vi.mock` works in browser mode; prefer it for parity with the existing tests.
6. **Determinism contract.** Real layout + real `ResizeObserver` settle asynchronously. Browser tests must **poll on the measured/observed value** (Vitest's retrying `expect.poll`/`expect.element`, or `vi.waitFor`) — never a fixed `sleep`. State this as the convention; it's the difference between a reliable suite and a flaky one. (The throwaway harness used fixed `waitForTimeout`s only because it was throwaway.)
7. **Promote the smoke test to the canonical example.** Mount `UnifiedTranscript`, seed a single long user message with compact mode on, and assert the **overflow invariant** that actually matters: the clip wrapper's `clientHeight < scrollHeight` and its computed `overflow-y: hidden`, and consequently the per-message toggle renders. Do **not** hardcode `max-height === 224px` — that depends on a 16px root font and would fail spuriously if `src/app.css` ever changed the root size; if a pixel value is genuinely wanted for calibration, derive it from the computed root font size. This proves CSS + measurement + the `clipOverflow` → toggle path end-to-end in WebKit, and is the example later tests copy.
8. **Make/CI integration, made self-sufficient.** Add a dedicated browser command + `make` target (mirror Makefile style), keep `make test`/`pnpm test` jsdom-only, and add the browser suite to the CI gate (`make check`). Two operational requirements, both load-bearing:
   - **The browser-test target must ensure the binary itself** (idempotent `playwright install webkit`, near-instant when already present). `make check` inlines its own `pnpm install --frozen-lockfile` and does **not** call `make install`, so putting the install only in `make install` would leave `make check` — the very gate this guards — able to hard-fail for anyone who hasn't manually installed the browser. (Adding it to `make install` too, for fresh-clone convenience, is fine; the target-level guarantee is the one that matters.)
   - **CI must cache the browser.** Add an `actions/cache` step on `~/Library/Caches/ms-playwright` (the **macOS** path — CI is `macos-15`; the Linux `~/.cache/ms-playwright` is wrong here) keyed on the resolved Playwright version + OS, so the ~100MB WebKit build downloads once instead of every push. The job has a 30-minute ceiling already doing Rust + lint + jsdom; a fresh download each run eats that budget and adds a network-flake surface to the gate.

Record the *why* (not just the *what*) in durable places: the project-split rationale and the "poll, don't sleep" rule belong in the browser-project setup file / a short header comment; the mock surface belongs next to the mount helper; the run policy and the "browser test" definition belong in AGENTS.md.

### Definition of done

- `@vitest/browser` + Playwright WebKit run a browser project that is separate from the jsdom project; the jsdom polyfills do not leak into it.
- The smoke test passes in WebKit and would **fail under jsdom** (it asserts real geometry) — confirming the layer actually exercises layout. (Sanity-check this once; it's the proof the infra is real.)
- `make test` / `pnpm test` stay jsdom-only/fast (pinned to `--project jsdom`); the browser suite runs via its own target and is included in `make check`.
- `make check` is **self-sufficient**: it succeeds on a checkout where the WebKit binary was never manually installed (the browser-test target ensures it), and CI runs green on `macos-15` with the browser **cached** (downloaded once, not per push).
- **Docs:** AGENTS.md gains a "browser test" entry in the test-type vocabulary and a one/two-line run policy (hermetic, in CI, separate target — contrast with live tests); README "How to run / test" mentions the new target. The mount-helper mock surface and the determinism rule are commented at their definitions.
- **Known limitation recorded:** browser tests are slower than jsdom and require a downloaded browser; this is why they are a separate target, not folded into `pnpm test`.
- **Open question to resolve in review:** confirm CI inclusion vs developer-local (see Chosen approach).

## Milestone 2 — Port the layout-coupled behaviors into permanent tests

### Goal & outcome

Convert the behaviors verified by hand in the WebKit walkthrough into permanent browser tests, so the bugs we fixed cannot silently return and future layout tweaks land with real coverage. Reuse the Milestone 1 harness and conventions throughout.

Functional outcomes — each is a behavior jsdom cannot check, now checked automatically:

- A response/message that genuinely overflows the clip shows a collapse toggle; one that fits shows none (no false toggles).
- A long **user message**, once expanded, **keeps** its toggle and can be re-collapsed (guards the measure-only-while-clipped fix).
- Scrolling up by **scrollbar or keyboard** (a bare `scroll` with unchanged height — no wheel/touch) unpins and **holds** position when new content arrives; a content-change-induced scroll (collapse clamp) does **not** unpin; scrolling back to the bottom re-pins.
- A tall **streaming** response is capped while live and, on completion, **stays pinned at the bottom** (the message's end stays in view — the reported "jerk to the top" bug).
- The **hidden-items indicator** shows the right summary (e.g. "1 tool call") on a collapsed standalone response and on **fan-out columns**, and clicking it reveals the hidden tools.
- The streaming **live cap is sized to the transcript area** (not the viewport) — it never exceeds the available height and scales with it.
- Expanding a mid-list message keeps its **footer anchored** on screen (the "the place I clicked stays put" contract).

### Implementation outline

These behaviors and their expected outcomes are the discussion-derived test matrix; the exact fixtures/assertions are the implementing agent's call against the harness. Sequencing is not load-bearing between cases, but the toggle/overflow and scroll cases are the highest-value (they encode the two shipped bugs) — write those first.

- Reuse the seeding patterns from `UnifiedTranscript.test.ts` (turn/agent/tool/fan-out builders, `fireTo` event injection for streaming) and the Milestone 1 mount helper. Compact mode is **on by default** (per the feature); set it explicitly per test where the case depends on it.
- For the **scroll** cases, the meaningful signal is the container's measured `scrollHeight - scrollTop - clientHeight` and whether it changes across a content mutation — assert on that (and on `scrollTop`), polling until layout settles. Reproduce the user-scroll-vs-layout-scroll distinction by dispatching a bare `scroll` after setting `scrollTop` (the scrollbar/keyboard shape) versus mutating content height.
- For **streaming completion**, drive the real lifecycle: seed/emit a streaming turn (overflowing), assert the live cap (`turn-live-scroll`) is present and pinned, then transition the turn to `complete` and assert the cap is gone and the view is still pinned with the finished response's end inside the viewport.
- For **fan-out**, seed two recipients sharing a `send_id` with tool-bearing responses and assert per-column indicators/toggles.
- Keep each test asserting one behavior; lean on measured geometry over class-string presence (a class can be present while its CSS doesn't apply — that was the original blind spot).
- Do **not** duplicate logic already well-covered in jsdom (e.g. which preview key a turn gets, label text composition). Browser tests are for the measured/rendered outcomes only.
- Two items deferred from the M1 review: (1) the `preview-clip` testid disambiguation — **resolved in M2**: all three clip sites share the `preview-clip` testid and specs scope to the owning turn when more than one is mounted (documented in the M1 harness header). (2) the per-spec IPC mock block — browser-mode `vi.mock` hoisting forces the ~3-line block into each spec, so it is copy-pasted across all browser specs. **Reconsidered and deferred again** (M2 review): the realistic drift is the component starting to touch a *new* IPC/native module beyond the current three (`@tauri-apps/api/core`, `@tauri-apps/api/event`, `$lib/native`) — and that fails **loudly** (the unmocked call throws at mount, across every spec), not silently, so a presence-grep guard buys little and wouldn't catch independent edits to the mock bodies anyway. **Trigger to revisit: when the component begins importing a new IPC/native module** (not spec count) — at that point add the mock for it to every spec and consider a shared assertion that the mocked surface matches the component's imports. Until then, the copy-paste stands.

### Definition of done

- Browser tests cover the seven outcomes above. The two guards for shipped bugs (user-message re-collapse retention; scrollbar/keyboard scroll-up hold) must each be **observed failing against the reverted fix** — not merely reasoned about. Both fixes are small and local, so a temporary revert to watch the test go red is cheap, and a regression test never seen red on the bug has unproven guard value (precisely how these bugs shipped). Record the observed-red in the test comment.

**As built (M2).** Six outcomes are exercised in WebKit; outcome 5 (hidden-items indicator label + click-to-reveal, standalone and fan-out) is **data-derived, not layout-coupled, and already thoroughly covered by the jsdom suite** (`UnifiedTranscript.test.ts`), so per the scope-discipline / no-duplication rule it was deliberately NOT re-implemented in the browser layer. Files: `transcript-overflow` (outcome 1), `user-recollapse` (outcome 2), `scroll-hold` (outcome 3), `streaming-pin` (outcomes 4 + 6), `footer-anchor` (outcome 7). The two observed-red guards map to the two small local fixes actually present in `UnifiedTranscript.svelte`: (a) the `clipOverflow`-retain-on-`destroy` fix → `user-recollapse` (toggle vanishes after expand under the revert); (b) the `onScroll` content-change gate (`scrollHeight === lastScrollHeight`) → the completion case in `streaming-pin` (view unpins and strands the response end under the revert). Note: the `scroll-hold` collapse case does NOT go red on its own under the gate revert, because WebKit's ResizeObserver re-anchor corrects scrollTop before the clamp's `scroll` fires — the gate's discriminating race only surfaces on stream completion, where `scrollSignal` and the clamp coincide; this is documented in both specs.
- All browser tests pass in WebKit in CI; the jsdom suite is unchanged and still green.
- Tests poll on measured values (no fixed sleeps); no flakiness across a few consecutive runs.
- Each test's intent is legible (what real-layout fact it asserts and why jsdom can't); the bug-guarding tests name the bug they guard.
- **Known limitation recorded:** these tests assert WebKit behavior; cross-engine differences (Chromium/Firefox) are not covered and are out of scope.

## Out of scope

- Porting the existing jsdom suite to the browser, or running non-layout component tests in WebKit.
- Cross-engine (Chromium/Firefox) coverage.
- End-to-end tests of the full Tauri app (real backend/IPC, real streaming agents) — this layer mounts components with mocked IPC, not the whole app.
- The further compact-transcript layout tweaks themselves — those land incrementally (implement → code review) and each adds its browser test here; they are not part of this plan.
