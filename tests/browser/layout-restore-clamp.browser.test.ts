import { afterEach, beforeEach, expect, test } from "vitest";
import { render } from "vitest-browser-svelte";
import { page } from "vitest/browser";
import { createRawSnippet } from "svelte";
import SidebarPanel from "$lib/components/ui/SidebarPanel.svelte";
import ResizableSidebarHost from "./ResizableSidebarHost.svelte";
import { _testing, GIT_DETAIL_MIN_WIDTH, layout, sidebarMaxWidth } from "$lib/layout.svelte";

// The persisted-layout viewport clamp is geometry-coupled: it reads the real
// window size and must produce a rendered rail that fits it. jsdom has no real
// viewport, so this runs in WebKit. Three layers are covered: the store's
// restart-path clamp (reload from a seeded blob), the CSS live bound (shrink
// the viewport after mount, then grow it back — the preference must survive),
// and the drag-start clamp (a drag after a shrink starts from the rendered
// width, not the stored one).

const STORAGE_KEY = "switchboard-layout";

const body = createRawSnippet(() => ({ render: () => `<div>contents</div>` }));

beforeEach(() => {
  localStorage.clear();
  _testing.reset();
});

afterEach(() => {
  localStorage.clear();
  _testing.reset();
});

test("a sidebar width saved on a larger monitor clamps to the live viewport on restore", async () => {
  localStorage.setItem(
    STORAGE_KEY,
    JSON.stringify({
      version: 1,
      layout: { projectsSidebar: { width: 5000, open: true } },
    }),
  );
  _testing.reloadFromStorage();

  // On a small viewport the SIDEBAR_MIN_WIDTH floor can exceed the 40% share;
  // the store keeps the rail usable rather than letterboxing it, so the
  // expectation mirrors that: the live max, never the saved 5000.
  const expected = sidebarMaxWidth();
  expect(expected).toBeLessThan(5000);
  expect(layout.projectsSidebarWidth).toBe(expected);

  render(SidebarPanel, {
    width: layout.projectsSidebarWidth,
    testid: "clamped-panel",
    children: body,
  });
  const panel = document.querySelector<HTMLElement>('[data-testid="clamped-panel"]');
  await expect.poll(() => panel?.getBoundingClientRect().width).toBe(expected);
});

test("a stored git detail width clamps proportionally to the live viewport", () => {
  localStorage.setItem(
    STORAGE_KEY,
    JSON.stringify({ version: 1, layout: { gitDetailWidth: 12_000 } }),
  );
  _testing.reloadFromStorage();
  expect(layout.gitDetailWidth).toBe(
    Math.max(GIT_DETAIL_MIN_WIDTH, Math.round(window.innerWidth * 0.85)),
  );
});

function hostSidebarWidth(): number {
  const el = document.querySelector<HTMLElement>('[data-testid="host-sidebar"]');
  return el === null ? 0 : el.getBoundingClientRect().width;
}

test("a mid-session shrink caps the rendered width live, and the preference re-expands on grow", async () => {
  await page.viewport(1200, 800);
  layout.projectsSidebarWidth = 480;
  render(ResizableSidebarHost, {});
  await expect.poll(() => hostSidebarWidth()).toBe(480);

  // Shrink: CSS caps to 40vw = 280 immediately; the stored value is untouched.
  await page.viewport(700, 800);
  await expect.poll(() => hostSidebarWidth()).toBe(280);
  expect(layout.projectsSidebarWidth).toBe(480);

  // Grow back: the preserved preference re-expands — the property a
  // store-rewriting resize listener would have destroyed.
  await page.viewport(1200, 800);
  await expect.poll(() => hostSidebarWidth()).toBe(480);
});

test("after a shrink, a drag starts from the rendered width — no jump to the stored one", async () => {
  await page.viewport(1200, 800);
  layout.projectsSidebarWidth = 480;
  render(ResizableSidebarHost, {});
  await expect.poll(() => hostSidebarWidth()).toBe(480);

  await page.viewport(700, 800);
  await expect.poll(() => hostSidebarWidth()).toBe(280);

  // Drag 40px left. Clamped start = 280 (rendered), so the draft is 240; an
  // unclamped start of 480 would draft 440 and stay CSS-pinned at 280.
  const handle = document.querySelector<HTMLElement>('[data-testid="host-sidebar-resizer"]');
  if (handle === null) throw new Error("resize handle not mounted");
  const from = handle.getBoundingClientRect().left;
  handle.dispatchEvent(
    new PointerEvent("pointerdown", { clientX: from, clientY: 150, bubbles: true }),
  );
  window.dispatchEvent(new PointerEvent("pointermove", { clientX: from - 40, clientY: 150 }));
  await expect.poll(() => hostSidebarWidth()).toBe(240);
  window.dispatchEvent(new PointerEvent("pointerup", { clientX: from - 40, clientY: 150 }));
  await expect.poll(() => layout.projectsSidebarWidth).toBe(240);
});
