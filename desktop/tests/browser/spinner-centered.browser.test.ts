import { expect, test, vi } from "vitest";

vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => vi.fn()) }));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => null),
  convertFileSrc: (p: string) => `asset://localhost/${p}`,
}));
vi.mock("$lib/native", () => ({ copyText: vi.fn(async () => undefined) }));

import { render } from "vitest-browser-svelte";
import ChipStateHost from "./ChipStateHost.svelte";

// A spinner rotates about the center of its box. Ink that is not centered in
// that box therefore travels a circle once per revolution — the indicator
// appears to swing around rather than turn in place. The check needs real SVG
// path measurement (`getTotalLength` / `getPointAtLength`), which jsdom does not
// implement, so it lives here.

/// Distance, in viewBox units, from an SVG path's centroid to the center of its
/// 24-unit viewBox. Sampled along the drawn length, so it measures the shape as
/// rendered rather than its control points.
function centroidOffset(path: SVGPathElement): number {
  const len = path.getTotalLength();
  let cx = 0;
  let cy = 0;
  const samples = 2000;
  for (let i = 0; i < samples; i++) {
    const pt = path.getPointAtLength((len * i) / samples);
    cx += pt.x;
    cy += pt.y;
  }
  return Math.hypot(cx / samples - 12, cy / samples - 12);
}

/// Border width the bare `Spinner` renders, which every other caller shows.
function defaultRingWidth(): number {
  const probe = document.createElement("span");
  probe.className = "block animate-spin rounded-full border-2";
  document.body.append(probe);
  const width = parseFloat(getComputedStyle(probe).borderTopWidth);
  probe.remove();
  return width;
}

test("the measure rejects the arc shape this bug came from", async () => {
  // Teeth for the assertion below: Lucide's `loader-circle` — the shape the
  // chip used — is a 288° arc whose centroid is over a unit off center, which at
  // the chip's 14px render is 1.2px of glyph swinging in a circle every second.
  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  svg.setAttribute("viewBox", "0 0 24 24");
  const path = document.createElementNS("http://www.w3.org/2000/svg", "path");
  path.setAttribute("d", "M21 12a9 9 0 1 1-6.219-8.56");
  svg.append(path);
  document.body.append(svg);

  expect(centroidOffset(path)).toBeGreaterThan(1);
  svg.remove();
});

test("the pending chip's indicator is a closed ring, centered on its own box", async () => {
  render(ChipStateHost, { name: "carl", readiness: "pending" });

  const host = document.querySelector('[data-testid="forward-source-state-carl"]') as HTMLElement;
  await expect.poll(() => host.firstElementChild !== null).toBe(true);

  // No drawn geometry that could sit off center…
  for (const path of host.querySelectorAll("path")) {
    expect(centroidOffset(path)).toBeLessThan(0.1);
  }

  // …because the indicator is a border ring: closed on all four sides and fully
  // rounded, so its ink is centered by construction whatever the angle.
  const ring = host.querySelector(".animate-spin") as HTMLElement;
  expect(ring).not.toBeNull();
  const style = getComputedStyle(ring);
  const widths = [
    style.borderTopWidth,
    style.borderRightWidth,
    style.borderBottomWidth,
    style.borderLeftWidth,
  ].map((w) => parseFloat(w));
  for (const width of widths) {
    expect(width).toBe(widths[0]);
    expect(width).toBeGreaterThan(0);
  }
  expect(widths[0]).toBe(defaultRingWidth());
  expect(parseFloat(style.borderTopLeftRadius)).toBeGreaterThanOrEqual(ring.clientWidth / 2);
  // The leading segment is what reads as motion, so the top edge must differ.
  expect(style.borderTopColor).not.toBe(style.borderLeftColor);
});
