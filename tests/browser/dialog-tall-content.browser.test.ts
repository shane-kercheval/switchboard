import { expect, test, vi } from "vitest";
import { page } from "vitest/browser";
import { render } from "vitest-browser-svelte";
import TallDialogHost from "./TallDialogHost.svelte";

vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => vi.fn()) }));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => null),
  convertFileSrc: (path: string) => `asset://localhost/${path}`,
}));

/// The Add Agent dialog used to run off **both** screen edges on a short
/// window: the card is centered with `-translate-y-1/2`, so an over-tall body
/// pushed the title bar above the viewport and the submit button below it, with
/// no way to reach either. Only a real engine computes this — jsdom reports
/// every rect as zero — so it lives here rather than in the jsdom suite.
test("a dialog taller than the viewport stays fully on screen", async () => {
  render(TallDialogHost);

  const card = page.getByTestId("dialog-content");
  await expect.element(card).toBeInTheDocument();

  await expect
    .poll(() => {
      const rect = (card.element() as HTMLElement).getBoundingClientRect();
      return {
        topOnScreen: rect.top >= 0,
        bottomOnScreen: rect.bottom <= window.innerHeight,
      };
    })
    .toEqual({ topOnScreen: true, bottomOnScreen: true });
});

/// The header must not be what scrolls. A dismiss control that scrolls out of
/// reach is the same trap as the original bug wearing a different hat — the
/// user can see the dialog but can't get out of it.
test("the title bar and close button stay pinned while the body scrolls", async () => {
  render(TallDialogHost);

  const body = page.getByTestId("tall-body");
  await expect.element(body).toBeInTheDocument();

  const scroller = (body.element() as HTMLElement).parentElement;
  expect(scroller, "body wrapper should exist").not.toBeNull();

  // The body genuinely overflows its container — otherwise the assertions
  // below would pass on a dialog that simply wasn't tall enough to test.
  await expect
    .poll(() => {
      const el = scroller as HTMLElement;
      return el.scrollHeight > el.clientHeight;
    })
    .toBe(true);

  const closeBefore = (
    page.getByTestId("dialog-close").element() as HTMLElement
  ).getBoundingClientRect().top;

  (scroller as HTMLElement).scrollTop = (scroller as HTMLElement).scrollHeight;

  await expect
    .poll(() => {
      const close = page.getByTestId("dialog-close").element() as HTMLElement;
      return Math.abs(close.getBoundingClientRect().top - closeBefore) < 1;
    })
    .toBe(true);

  // And scrolling actually reached the far end of the content.
  await expect
    .poll(() => {
      const last = page.getByTestId("body-last").element() as HTMLElement;
      return last.getBoundingClientRect().bottom <= window.innerHeight + 1;
    })
    .toBe(true);
});
