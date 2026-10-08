import { expect, test, vi } from "vitest";
import { render } from "vitest-browser-svelte";

vi.mock("$lib/native", () => ({ copyText: vi.fn(async () => undefined) }));

import FindingsCard from "$lib/components/FindingsCard.svelte";
import type { FindingsFacet } from "$lib/types";

// The row's metadata (badge, category, location) must never squeeze the
// finding's title out: in a narrow fan-out column a long path, a long category
// and the widest badge together used to leave the title zero-width and push
// the row past its column. Layout-coupled, so it lives in the browser suite.
test("a finding's title stays readable in a narrow column with long metadata", async () => {
  const host = document.createElement("div");
  host.style.width = "320px";
  document.body.appendChild(host);
  const facet: FindingsFacet = {
    facet_kind: "findings",
    level: "high",
    findings: [
      {
        file: "desktop/tests/browser/perf-baseline.browser.test.ts",
        line: 24,
        summary: "Perf harness run instructions still assume the repo root.",
        failure_scenario: "Following them from the root fails.",
        category: "a-very-long-category-slug-for-test-cases",
        outcome: "no_change_needed",
      },
    ],
    text: "REVIEW",
  };

  render(FindingsCard, { props: { facet }, target: host });

  const title = host.querySelector<HTMLElement>('[data-testid="finding-title"]')!;
  const row = host.querySelector<HTMLElement>('[data-testid="finding-toggle"]')!;
  await expect.poll(() => title.getBoundingClientRect().width).toBeGreaterThanOrEqual(60);
  expect(row.scrollWidth).toBeLessThanOrEqual(row.clientWidth + 1);
  expect(row.getBoundingClientRect().right).toBeLessThanOrEqual(
    host.getBoundingClientRect().right + 1,
  );
});
