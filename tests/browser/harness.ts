import { page } from "vitest/browser";
import type { AgentRecord, NormalizedEvent } from "$lib/types";
import * as stateMod from "$lib/state/index.svelte";
import type { Turn } from "$lib/state/index.svelte";
import { _testing as previewState } from "$lib/state/transcriptPreview.svelte";

// ---------------------------------------------------------------------------
// Mount/seed helpers + the canonical mock surface for browser-project tests.
//
// MOCK SURFACE (must be declared in the SPEC FILE). In Vitest *browser* mode,
// `vi.mock` is hoisted only within the file that calls it; a `vi.mock` placed in
// this helper does NOT register before the component graph resolves, and a
// factory that references an imported helper trips the "no top-level variables"
// hoist guard. So every browser spec declares the block below at its own top
// level. `@tauri-apps/api/core`, `@tauri-apps/api/event`, and `$lib/native` are
// exactly the IPC/native surface UnifiedTranscript (and its children) touch on
// mount — established empirically. Copy this:
//
//   import { vi } from "vitest";
//   // `vi.hoisted` is the hoist-safe way to share the listener map with fireTo;
//   // omit it if the spec doesn't drive streaming.
//   const { listeners } = vi.hoisted(() => ({ listeners: new Map() }));
//   vi.mock("@tauri-apps/api/event", () => ({
//     listen: vi.fn(async (name: string, cb) => { listeners.set(name, cb); return vi.fn(); }),
//   }));
//   vi.mock("@tauri-apps/api/core", () => ({
//     invoke: vi.fn(async () => null),
//     convertFileSrc: (p: string) => `asset://localhost/${p}`,
//   }));
//   vi.mock("$lib/native", () => ({ copyText: vi.fn(async () => undefined) }));
//
// then drive streaming with `fireTo(listeners, channel, event)`.
//
// COMPOSE-SURFACE SPECS need MORE than this surface: ComposeBar's graph pulls
// in `@tauri-apps/api/webview`, which must also be mocked, and its mounts live
// in ./composeMount (NOT ./mount) so that graph stays out of every other
// spec's imports — see composeMount.ts for the rationale.
//
// PATTERNS for new browser specs:
// - Reset shared module state in `beforeEach` (call `resetState()`), NOT
//   `afterEach`. Browser-mode tests in a file share one long-lived page, so a
//   clean slate is most reliably guaranteed *before* each test runs;
//   `vitest-browser-svelte`'s own `beforeEach(cleanup)` (registered at import,
//   so it runs first) unmounts the prior component before the reset.
// - When a spec seeds MORE THAN ONE message/column, `data-testid="preview-clip"`
//   matches every clip on the page and `page.getByTestId("preview-clip")` would
//   silently resolve the first. Scope to the owning turn —
//   `page.getByTestId("turn").nth(i).getByTestId("preview-clip")` — or add a
//   type-specific testid when a spec needs to disambiguate user vs agent vs column
//   clips. A spec that seeds a single turn can use the bare locator.
// ---------------------------------------------------------------------------

/** The unified transcript's outer scroll container. */
export function transcriptContainer(): HTMLElement {
  return page.getByTestId("unified-transcript").element() as HTMLElement;
}

/**
 * Outer-scroll distance from the bottom — the measure every scroll spec
 * asserts pinned/unpinned thresholds against. Shared so all specs agree on
 * what "distance" means.
 */
export function distanceFromBottom(): number {
  const c = transcriptContainer();
  return c.scrollHeight - c.scrollTop - c.clientHeight;
}

/**
 * Deliver a normalized event to a captured per-agent listener (streaming drive).
 * Pass the spec's `vi.hoisted` listener map (see header) — the browser-mode
 * counterpart of the jsdom suite's listener map.
 */
export function fireTo(
  listeners: Map<string, (e: { payload: NormalizedEvent }) => void>,
  channel: string,
  event: NormalizedEvent,
): void {
  const cb = listeners.get(channel);
  if (cb === undefined) throw new Error(`no listener for ${channel}`);
  cb({ payload: event });
}

/** Register an agent so the component subscribes/renders it (passthrough). */
export async function registerAgent(agent: AgentRecord): Promise<void> {
  await stateMod.registerAgent(agent);
}

/** Seed an agent's transcript. Routed through `setTranscript` (the store's
 * single-writer contract): browser specs re-seed mid-test to simulate
 * streaming, and the revision bump is what drives the re-anchor effect. */
export function seedTurns(agentId: string, turns: Turn[]): void {
  stateMod.setTranscript(agentId, turns);
}

/** Reset shared module state between tests (transcripts/runtimes + compact). */
export function resetState(): void {
  stateMod._testing.reset();
  previewState.reset();
}

// Re-exported for specs that need richer seeding than seedTurns (runtimes,
// dispatch helpers) — the same module the jsdom suite mutates directly.
export * as state from "$lib/state/index.svelte";
