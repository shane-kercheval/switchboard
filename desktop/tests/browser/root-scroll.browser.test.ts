import { expect, test } from "vitest";
import { page } from "vitest/browser";
import { render } from "vitest-browser-svelte";
import RootScrollHost from "./RootScrollHost.svelte";

/// The desktop shell delegates scrolling to explicit inner regions. If a
/// trackpad gesture reaches one of their edges, the document must remain locked
/// so the entire app cannot drift inside the WKWebView viewport.
test("inner wheel scrolling cannot escape to the document", async () => {
  const app = document.createElement("div");
  app.id = "app";
  document.body.append(app);

  const documentOverflow = document.createElement("div");
  documentOverflow.style.height = "4000px";
  document.body.append(documentOverflow);

  try {
    render(RootScrollHost, { target: app });

    const innerLocator = page.getByTestId("inner-scroll-region");
    const inner = innerLocator.element() as HTMLElement;
    const root = document.scrollingElement as HTMLElement;

    await expect.poll(() => app.clientHeight).toBe(window.innerHeight);
    await expect.poll(() => inner.scrollHeight > inner.clientHeight).toBe(true);
    await expect.poll(() => root.scrollHeight > root.clientHeight).toBe(true);

    root.scrollTop = 0;
    await innerLocator.wheel({ delta: { y: 160 } });
    await expect.poll(() => inner.scrollTop).toBeGreaterThan(0);

    inner.scrollTop = inner.scrollHeight;
    await expect.poll(() => inner.scrollTop).toBeGreaterThan(0);
    await innerLocator.wheel({ delta: { y: 240 } });
    await expect.poll(() => root.scrollTop).toBe(0);

    // Root rubber-banding is a visual boundary action: WebKit does not expose
    // it through scroll geometry, so the applied property is the observable
    // contract for that half of the fix.
    expect(getComputedStyle(document.documentElement).overscrollBehaviorY).toBe("none");
  } finally {
    documentOverflow.remove();
    app.remove();
  }
});
