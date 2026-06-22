import { beforeEach, expect, test, vi } from "vitest";

vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => vi.fn()) }));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => null),
  convertFileSrc: (p: string) => `asset://localhost/${p}`,
}));
vi.mock("$lib/native", () => ({ copyText: vi.fn(async () => undefined) }));

import { mountTranscript } from "./mount";
import { registerAgent, seedTurns, resetState } from "./harness";
import { ALICE, PROJECT_ID } from "./fixtures";
import { setProjectCompact } from "$lib/state/transcriptPreview.svelte";
import { buildLargeTranscript } from "$lib/dev/largeTranscript";
import { buildUnifiedRows, groupRenderBlocks } from "$lib/state/unified";
import { transcriptReducer } from "$lib/state/reducers";
import { renderMarkdown } from "$lib/markdown";
import type { Turn } from "$lib/state/types";

// MANUAL MEASUREMENT HARNESS — not part of CI's behavioral suite. Wall-clock
// numbers are never asserted (per the plan's conventions); this file gathers
// them on demand, in the same real WebKit the behavioral suite uses:
//
//   VITE_PERF=1 pnpm vitest run --project browser tests/browser/perf-baseline.browser.test.ts
//
// Results are dumped through a deliberately failing assertion (browser-mode
// console output doesn't reach the terminal reliably). Without VITE_PERF the
// tests skip, so `make check` is unaffected.
//
// NOTE: render-windowing caps the mounted transcript at INITIAL_WINDOW blocks,
// so the "windowed-tail" scenarios below seed a deep history but mount only the
// tail window — the relayout numbers reflect the windowed DOM (now the realistic
// production case), not all 300 exchanges. To measure the old unwindowed upper
// bound a run would have to defeat the window.
//
// What it measures and which decisions it feeds (see the perf plan's M3/M5
// sections, docs/implementation_plans/2026-06-09-performance-improvements.md):
// - The compose autosize's exact per-keystroke layout operation (height-reset
//   write + scrollHeight read) against {small, large} × {containment on/off}
//   × {compact on/off} — the M4 gate and the M1 rAF contingency. Containment
//   "off" is a same-build A/B via an injected style override, isolating M3's
//   contribution.
// - Per-chunk cost of each streaming pipeline stage on the large fixture
//   (reducer, rows rebuild, scrollSignal walk, markdown re-parse by segment
//   size) — M5's step-0 baseline that gates its Fixes 3/4.

const PERF = import.meta.env.VITE_PERF === "1";

/// WebKit quantizes performance.now() to ~1ms, so each sample times a BATCH
/// of reps and divides — the result is a mean per-op, not a distribution.
function timeBatched(reps: number, run: () => void): number {
  const t0 = performance.now();
  for (let i = 0; i < reps; i++) run();
  return (performance.now() - t0) / reps;
}

beforeEach(() => {
  resetState();
  document.getElementById("perf-no-containment")?.remove();
});

test.runIf(PERF)("forced-layout cost per keystroke-equivalent", async () => {
  const results: string[] = [];

  const measureScenario = async (
    label: string,
    exchanges: number,
    opts: { containment: boolean; compact: boolean },
  ): Promise<void> => {
    resetState();
    document.getElementById("perf-no-containment")?.remove();
    setProjectCompact(PROJECT_ID, opts.compact);
    if (!opts.containment) {
      const style = document.createElement("style");
      style.id = "perf-no-containment";
      style.textContent =
        '[data-testid="transcript-block"] { content-visibility: visible !important; }';
      document.head.append(style);
    }
    await registerAgent(ALICE);
    seedTurns(ALICE.id, buildLargeTranscript({ agentIds: [ALICE.id], exchanges })[ALICE.id]!);
    const r = mountTranscript({ projectId: PROJECT_ID, agents: [ALICE] });
    await expect
      .poll(() => document.querySelectorAll("[data-testid=transcript-block]").length, {
        timeout: 20_000,
      })
      .toBeGreaterThan(0);
    // Let layout fully settle before measuring.
    await new Promise((res) => requestAnimationFrame(() => requestAnimationFrame(res)));

    // The compose bar's exact autosize operation, against a sibling textarea
    // (the compose bar sits outside the transcript's scroll container).
    const ta = document.createElement("textarea");
    ta.style.cssText = "position:fixed;bottom:0;left:0;width:60%;font-size:14px;padding:4px;";
    ta.value = "the quick brown fox\njumps over\nthe lazy dog";
    document.body.append(ta);

    // (i) Keystroke-faithful: mutate the value (what a keypress does), then the
    // autosize op — height-reset write + scrollHeight read forces the flush.
    const keystroke = (): void => {
      ta.value += "x";
      ta.style.height = "auto";
      const h = ta.scrollHeight;
      ta.style.height = `${h}px`;
    };
    timeBatched(20, keystroke); // warmup
    const perKeystroke = timeBatched(200, keystroke);

    // (ii) Full-relayout bound: alternate the transcript container's width so
    // the whole transcript participates in the flush — the upper bound that
    // containment exists to cut.
    const container = document.querySelector('[data-testid="unified-transcript"]') as HTMLElement;
    let flip = false;
    const fullRelayout = (): void => {
      flip = !flip;
      container.style.paddingRight = flip ? "33px" : "32px";
      void container.offsetHeight;
    };
    timeBatched(4, fullRelayout); // warmup
    const perRelayout = timeBatched(30, fullRelayout);

    ta.remove();
    r.unmount();
    results.push(
      `${label}: keystroke ${perKeystroke.toFixed(3)}ms, full-relayout ${perRelayout.toFixed(2)}ms`,
    );
  };

  await measureScenario("small (10x) containment+compact", 10, {
    containment: true,
    compact: true,
  });
  await measureScenario("windowed-tail (300x seed) containment+compact", 300, {
    containment: true,
    compact: true,
  });
  await measureScenario("windowed-tail (300x seed) NO-containment compact", 300, {
    containment: false,
    compact: true,
  });
  await measureScenario("windowed-tail (300x seed) containment compact-OFF", 300, {
    containment: true,
    compact: false,
  });
  await measureScenario("windowed-tail (300x seed) NO-containment compact-OFF", 300, {
    containment: false,
    compact: false,
  });

  expect(results.join(" | ")).toBe("__REPORT__");
});

test.runIf(PERF)("streaming pipeline per-chunk stage costs (M5 step 0)", () => {
  const results: string[] = [];
  const agentId = ALICE.id;
  const history = buildLargeTranscript({ agentIds: [agentId], exchanges: 300 })[agentId]!;
  const liveText = Array.from({ length: 60 }, (_, i) => `Streaming line ${i + 1}.`).join("\n");
  const turns: Turn[] = [
    ...history,
    {
      role: "agent",
      turn_id: "live-1",
      agent_id: agentId,
      started_at: "2026-05-16T00:00:00Z",
      status: "streaming",
      items: [{ item_kind: "text", kind: "text", text: liveText }],
    },
  ];
  const knownIds = new Set([agentId]);

  const bench = (label: string, run: () => void): void => {
    timeBatched(10, run); // warmup
    results.push(`${label}: ${timeBatched(100, run).toFixed(3)}ms/op`);
  };

  bench("reducer content_chunk", () => {
    transcriptReducer(
      turns,
      { type: "content_chunk", turn_id: "live-1", kind: "text", text: "another chunk of text " },
      agentId,
      "2026-05-16T00:00:01.000Z",
    );
  });

  bench("buildUnifiedRows+groupRenderBlocks", () => {
    groupRenderBlocks(buildUnifiedRows(turns, [], knownIds), [agentId]);
  });

  const rows = buildUnifiedRows(turns, [], knownIds);
  bench("scrollSignal content walk [HISTORICAL — removed by M5 Fix 2]", () => {
    // The pre-Fix-2 digest derived. Production now reads an O(1) revision
    // counter instead (`getTranscriptRevision`); this benchmark is kept only
    // as the step-0 baseline that justified the change — it does NOT reflect
    // current per-event cost.
    let n = rows.length;
    for (const row of rows) {
      if (row.kind === "user") n += row.text.length;
      else if (row.kind === "outcome") n += 0;
      else {
        n += row.turn.items.length;
        for (const item of row.turn.items) {
          if (item.item_kind === "text") n += item.text.length;
          else n += item.output?.length ?? 0;
        }
      }
    }
    if (n < 0) throw new Error("unreachable");
  });

  for (const size of [500, 2000, 8000] as const) {
    const segment = liveText.repeat(Math.ceil(size / liveText.length)).slice(0, size);
    bench(`renderMarkdown live segment ${size} chars`, () => {
      renderMarkdown(segment);
    });
  }

  expect(results.join(" | ")).toBe("__REPORT__");
});
