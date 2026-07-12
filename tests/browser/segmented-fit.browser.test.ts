import { expect, test } from "vitest";
import { render } from "vitest-browser-svelte";
import { page } from "vitest/browser";
import { EFFORT_OPTIONS, MODEL_OPTIONS } from "$lib/agentSelection";
import SegmentedFitHost from "./SegmentedFitHost.svelte";

// The effort/model segmented controls must stay on ONE row and never clip their
// labels — the whole reason we shrink the font past five options instead of
// wrapping to a second row (which read as a broken pill). jsdom sees no layout,
// so this is a real-WebKit geometry check. Width is the tightest real container:
// the add-agent card is `max-w-lg` (512px) with `p-5`, so the control renders at
// ~472px; the change dialogs (`max-w-lg`, `p-4`) are slightly wider (~480px).
const TIGHTEST_INNER_WIDTH = 472;

async function assertSingleRowNoClip(testid: string): Promise<void> {
  const buttons = Array.from(
    (page.getByTestId(testid).element() as HTMLElement).querySelectorAll<HTMLElement>(
      '[role="radio"]',
    ),
  );
  expect(buttons.length).toBeGreaterThan(0);

  // Single row: every segment shares the first segment's vertical offset.
  const firstTop = buttons[0]!.offsetTop;
  for (const b of buttons) {
    expect(b.offsetTop).toBe(firstTop);
  }

  // No clipped labels: a truncated segment has content wider than its box.
  for (const b of buttons) {
    expect(b.scrollWidth).toBeLessThanOrEqual(b.clientWidth + 1);
  }
}

test("Codex's eight effort levels fit one row without clipping at the dialog width", async () => {
  render(SegmentedFitHost, {
    props: { width: TIGHTEST_INNER_WIDTH, options: EFFORT_OPTIONS.codex, value: "medium" },
  });
  await expect.element(page.getByTestId("fit")).toBeInTheDocument();
  await expect
    .poll(() => (page.getByTestId("fit").element() as HTMLElement).offsetHeight)
    .toBeGreaterThan(0);
  await assertSingleRowNoClip("fit");
});

test("Codex's model pills fit one row without clipping at the dialog width", async () => {
  render(SegmentedFitHost, {
    props: { width: TIGHTEST_INNER_WIDTH, options: MODEL_OPTIONS.codex, value: "gpt-5.6-terra" },
  });
  await expect.element(page.getByTestId("fit")).toBeInTheDocument();
  await expect
    .poll(() => (page.getByTestId("fit").element() as HTMLElement).offsetHeight)
    .toBeGreaterThan(0);
  await assertSingleRowNoClip("fit");
});
