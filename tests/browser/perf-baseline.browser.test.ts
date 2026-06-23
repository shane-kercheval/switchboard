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
// What it measures and which decisions it feeds:
// - The compose autosize's exact per-keystroke layout operation (height-reset
//   write + scrollHeight read) against {small, windowed-tail} × {compact on/off}.
//   (The former containment on/off A/B is gone — content-visibility containment
//   was removed once render-windowing took over bounding the mounted set.)
// - Per-chunk cost of each streaming pipeline stage on the large fixture
//   (reducer, rows rebuild, scrollSignal walk, markdown re-parse by segment
//   size) — the streaming-pipeline baseline that gates the streaming fixes.

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
});

test.runIf(PERF)("forced-layout cost per keystroke-equivalent", async () => {
  const results: string[] = [];

  const measureScenario = async (
    label: string,
    exchanges: number,
    opts: { compact: boolean },
  ): Promise<void> => {
    resetState();
    setProjectCompact(PROJECT_ID, opts.compact);
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
    // the whole mounted window participates in the flush — the upper bound the
    // compose textarea pays, now bounded by render-windowing rather than cut by
    // containment.
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

  await measureScenario("small (10x) compact", 10, { compact: true });
  await measureScenario("windowed-tail (300x seed) compact", 300, { compact: true });
  await measureScenario("windowed-tail (300x seed) compact-OFF", 300, { compact: false });

  expect(results.join(" | ")).toBe("__REPORT__");
});

test.runIf(PERF)("streaming pipeline per-chunk stage costs (baseline)", () => {
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
  bench("scrollSignal content walk [HISTORICAL — since removed]", () => {
    // The pre-Fix-2 digest derived. Production now reads an O(1) revision
    // counter instead (`getTranscriptRevision`); this benchmark is kept only
    // as the step-0 baseline that justified the change — it does NOT reflect
    // current per-event cost.
    let n = rows.length;
    for (const row of rows) {
      if (row.kind === "user") n += row.text.length;
      else if (row.kind === "outcome") n += 0;
      else if (row.kind === "system_marker") n += 0;
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
