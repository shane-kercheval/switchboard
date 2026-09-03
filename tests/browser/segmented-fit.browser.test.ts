import { expect, test } from "vitest";
import { render } from "vitest-browser-svelte";
import { page } from "vitest/browser";
import { EFFORT_OPTIONS, MODEL_OPTIONS } from "$lib/agentSelection";
import AgentSelectionEditor from "$lib/components/AgentSelectionEditor.svelte";
import SegmentedFitHost from "./SegmentedFitHost.svelte";

// The effort/model segmented controls must stay on ONE row and never clip their
// labels — the whole reason we shrink the font past five options instead of
// wrapping to a second row (which reads as a broken pill). jsdom sees no layout,
// so this is a real-WebKit geometry check.
//
// Every one of these controls renders inside the add-agent or model-settings
// dialog. Both are `max-w-[612px]` with `p-4`, so the control gets ~580px.
//
// `max-w` is a ceiling, not a width: the card is `w-[calc(100vw-2rem)]`, so a
// window under ~564px shrinks it further and labels begin to truncate. That is
// an accepted limit, not a covered case — measured crossover for the widest set
// (Antigravity's five models) is between 500px and 472px of inner width.
const DIALOG_INNER_WIDTH = 580;

test("multi-select quick choices divide the full track into equal segments", async () => {
  render(AgentSelectionEditor, {
    props: {
      harness: "claude_code",
      selection: {
        model: "opus",
        effort: "high",
        model_choices: ["opus", "sonnet"],
        effort_choices: ["high", "medium"],
      },
      context: "default",
      onChange: () => {},
      testidPrefix: "multi-fit",
    },
  });
  await expect.element(page.getByTestId("multi-fit-model-choices")).toBeInTheDocument();

  const track = page.getByTestId("multi-fit-model-choices").element() as HTMLElement;
  const buttons = Array.from(track.querySelectorAll<HTMLElement>("button"));
  expect(buttons).toHaveLength(MODEL_OPTIONS.claude_code.length);
  const widths = buttons.map((button) => button.getBoundingClientRect().width);
  expect(Math.max(...widths) - Math.min(...widths)).toBeLessThanOrEqual(1);
  const trackRect = track.getBoundingClientRect();
  const firstRect = buttons[0]!.getBoundingClientRect();
  const lastRect = buttons.at(-1)!.getBoundingClientRect();
  expect(firstRect.left - trackRect.left).toBeLessThanOrEqual(4);
  expect(trackRect.right - lastRect.right).toBeLessThanOrEqual(4);
});

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
    props: { width: DIALOG_INNER_WIDTH, options: EFFORT_OPTIONS.codex, value: "medium" },
  });
  await expect.element(page.getByTestId("fit")).toBeInTheDocument();
  await expect
    .poll(() => (page.getByTestId("fit").element() as HTMLElement).offsetHeight)
    .toBeGreaterThan(0);
  await assertSingleRowNoClip("fit");
});

test("Codex's model pills fit one row without clipping at the dialog width", async () => {
  render(SegmentedFitHost, {
    props: { width: DIALOG_INNER_WIDTH, options: MODEL_OPTIONS.codex, value: "gpt-5.6-terra" },
  });
  await expect.element(page.getByTestId("fit")).toBeInTheDocument();
  await expect
    .poll(() => (page.getByTestId("fit").element() as HTMLElement).offsetHeight)
    .toBeGreaterThan(0);
  await assertSingleRowNoClip("fit");
});

test("Antigravity's five model pills fit one row without clipping at the dialog width", async () => {
  render(SegmentedFitHost, {
    props: {
      width: DIALOG_INNER_WIDTH,
      options: MODEL_OPTIONS.antigravity,
      value: "gemini-3.1-pro",
    },
  });
  await expect.element(page.getByTestId("fit")).toBeInTheDocument();
  await expect
    .poll(() => (page.getByTestId("fit").element() as HTMLElement).offsetHeight)
    .toBeGreaterThan(0);
  await assertSingleRowNoClip("fit");
});

test("Claude's five effort levels fit one row without clipping at the dialog width", async () => {
  render(SegmentedFitHost, {
    props: { width: DIALOG_INNER_WIDTH, options: EFFORT_OPTIONS.claude_code, value: "high" },
  });
  await expect.element(page.getByTestId("fit")).toBeInTheDocument();
  await expect
    .poll(() => (page.getByTestId("fit").element() as HTMLElement).offsetHeight)
    .toBeGreaterThan(0);
  await assertSingleRowNoClip("fit");
});
