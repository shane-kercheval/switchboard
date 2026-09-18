import { expect, test } from "vitest";
import { page } from "vitest/browser";
import { render } from "vitest-browser-svelte";
import AgentEnvironmentHost from "./AgentEnvironmentHost.svelte";
import type { SessionInventory } from "$lib/types";

/// The default and minimum agent-sidebar widths (`layout.svelte`'s
/// `SIDEBAR_MIN_WIDTH` and the agents sidebar default).
const DEFAULT_WIDTH = 240;
const MIN_WIDTH = 200;

/// A realistic worst case rather than a contrived one: the counts from the
/// probe account that drove this milestone's design — seven MCP servers with
/// two needing auth, plus every other section populated.
const BUSY: SessionInventory = {
  mcp_servers: [
    { name: "a", status: "connected" },
    { name: "b", status: "connected" },
    { name: "c", status: "connected" },
    { name: "d", status: "connected" },
    { name: "e", status: "connected" },
    { name: "f", status: "needs-auth" },
    { name: "g", status: "needs-auth" },
  ],
  agents: ["a", "b", "c", "d", "e", "f"],
  plugins: [{ name: "p" }],
  skills: Array.from({ length: 30 }, (_, i) => ({ name: `s${i}` })),
  memory_paths: ["/m"],
};

/// How many pixels of the element's text are clipped. Zero means it fits.
function overflow(testid: string): number {
  const el = page.getByTestId(testid).element() as HTMLElement;
  return el.scrollWidth - el.clientWidth;
}

test("the collapsed counts line reports its overflow at the card's widths", async () => {
  // Measured, not estimated — the preceding milestone's label estimates were
  // wrong by 3-7x. This test's job is to make the fit a recorded number that
  // moves visibly if the summary's wording changes, not to assert a design.
  render(AgentEnvironmentHost, { props: { width: DEFAULT_WIDTH, inventory: BUSY } });

  await expect.element(page.getByTestId("agent-env-summary")).toBeInTheDocument();
  const text = (page.getByTestId("agent-env-summary").element() as HTMLElement).textContent;
  expect(text).toBe("MCP 7 · 2 need auth · Agents 6 · Plugins 1 · Skills 30 · Memory 1");

  // **No horizontal clipping, at any content length.** The busiest realistic
  // summary needs ~367px against the ~224px a default-width card gives it, so
  // it wraps. Truncating instead dropped the Skills and Memory counts off the
  // card entirely (measured: 127px clipped), which is the one thing this row
  // is specifically not allowed to do.
  expect(overflow("agent-env-summary")).toBe(0);

  // It wraps, rather than overflowing the row or pushing the chevron out of
  // the card — a chevron off the card would make the row unopenable.
  const summary = page.getByTestId("agent-env-summary").element() as HTMLElement;
  const oneLine = Number.parseFloat(getComputedStyle(summary).lineHeight);
  expect(summary.getBoundingClientRect().height).toBeGreaterThan(oneLine * 1.5);
  const row = page.getByTestId("agent-env-toggle").element() as HTMLElement;
  expect(row.scrollWidth - row.clientWidth).toBeLessThanOrEqual(1);
});

test("a typical summary fits the default card width", async () => {
  // The common case is not the busy one: an agent with a couple of servers
  // and no plugins reads fully without expanding.
  render(AgentEnvironmentHost, {
    props: {
      width: DEFAULT_WIDTH,
      inventory: {
        mcp_servers: [
          { name: "a", status: "connected" },
          { name: "b", status: "needs-auth" },
        ],
        skills: [{ name: "s" }],
      },
    },
  });

  await expect.element(page.getByTestId("agent-env-summary")).toBeInTheDocument();
  expect((page.getByTestId("agent-env-summary").element() as HTMLElement).textContent).toBe(
    "MCP 2 · 1 need auth · Skills 1",
  );
  expect(overflow("agent-env-summary")).toBe(0);
});

test("nothing clips at the minimum card width either", async () => {
  // 200px is the sidebar's floor, where M1's context-meter labels still clip
  // by a few pixels. The counts line must not join them: it wraps to however
  // many lines it needs.
  render(AgentEnvironmentHost, { props: { width: MIN_WIDTH, inventory: BUSY } });

  await expect.element(page.getByTestId("agent-env-summary")).toBeInTheDocument();
  expect(overflow("agent-env-summary")).toBe(0);
  const row = page.getByTestId("agent-env-toggle").element() as HTMLElement;
  expect(row.scrollWidth - row.clientWidth).toBeLessThanOrEqual(1);
});
