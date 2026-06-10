import { expect, test } from "vitest";
import { render } from "vitest-browser-svelte";
import { page } from "vitest/browser";
import Probe from "./Probe.svelte";

// Compatibility gate: the @vitest/browser + vitest-browser-svelte + WebKit stack
// mounts a real Svelte 5 component. If this fails, the version stack is
// misaligned — fix that before building the harness/conventions.
test("mounts a Svelte component in real WebKit", async () => {
  render(Probe, { label: "webkit ok" });
  const probe = page.getByTestId("probe");
  await expect.element(probe).toBeVisible();
  await expect.element(probe).toHaveTextContent("webkit ok");
});
