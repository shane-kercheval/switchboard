import { expect, test, vi } from "vitest";
import { page, userEvent } from "vitest/browser";
import { render } from "vitest-browser-svelte";
import DropdownOverflowHost from "./DropdownOverflowHost.svelte";

vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => vi.fn()) }));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => null),
  convertFileSrc: (path: string) => `asset://localhost/${path}`,
}));

/// Menu content is caller-supplied and unbounded, so a long menu used to run
/// past the viewport with its clipped rows unreachable — the same dead end as a
/// row that can't take focus. The shared content now caps its height and scrolls.
/// jsdom reports every rect as zero, so this can only live here.
test("a menu taller than the viewport scrolls instead of overflowing it", async () => {
  render(DropdownOverflowHost);

  const content = page.getByTestId("overflow-content");
  await expect.element(content).toBeInTheDocument();

  // Contained: the menu's own box fits on screen...
  await expect
    .poll(() => {
      const rect = (content.element() as HTMLElement).getBoundingClientRect();
      return rect.top >= -1 && rect.bottom <= window.innerHeight + 1;
    })
    .toBe(true);

  // ...and it is genuinely scrolling rather than having silently dropped rows.
  await expect
    .poll(() => {
      const el = content.element() as HTMLElement;
      return el.scrollHeight > el.clientHeight;
    })
    .toBe(true);

  // Keyboard navigation must reach the last row *and* land it inside the
  // scrollport — clipping breaks exactly that. Driven by `End` rather than a
  // counted run of ArrowDowns: bits-ui's roving focus and typeahead make
  // keystroke counts brittle, and the property under test is "focus-driven
  // navigation reaches a row inside the visible bounds", not the item count.
  const last = page.getByTestId("row-item-59");
  const lastEl = last.element() as HTMLElement;
  // Walk the item ring until focus reaches the final row. Bounded, but the
  // assertion is "navigation reaches it", never a specific keystroke count —
  // bits-ui's roving focus and typeahead make counts brittle in a real engine.
  for (let i = 0; i < 200 && document.activeElement !== lastEl; i += 1) {
    await userEvent.keyboard("{ArrowDown}");
  }
  expect(document.activeElement).toBe(lastEl);
  // Reached *and* inside the scrollport — clipping breaks exactly this pairing.
  await expect
    .poll(() => {
      const menu = (content.element() as HTMLElement).getBoundingClientRect();
      const row = lastEl.getBoundingClientRect();
      return row.top >= menu.top - 1 && row.bottom <= menu.bottom + 1;
    })
    .toBe(true);
});
