import { beforeEach, expect, test, vi } from "vitest";
import { page } from "vitest/browser";

const browserState = vi.hoisted(() => ({
  versions: {
    claude_code: "1.2.3",
    codex: "1.2.3",
    antigravity: null,
  } as Record<string, string | null>,
}));

vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => vi.fn()) }));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (cmd: string, args?: { harness?: string }) => {
    if (cmd === "get_harness_install_status") {
      const version = browserState.versions[args?.harness ?? ""] ?? null;
      return {
        installed: version !== null,
        version,
        path_source: "login_shell",
      };
    }
    return null;
  }),
  convertFileSrc: (path: string) => `asset://localhost/${path}`,
}));

import { render } from "vitest-browser-svelte";
import WelcomeScreenHost from "./WelcomeScreenHost.svelte";
import { _testing as availabilityTesting } from "$lib/harnessAvailability.svelte";

beforeEach(() => {
  availabilityTesting.reset();
  browserState.versions.claude_code = "1.2.3";
  browserState.versions.codex = "1.2.3";
  browserState.versions.antigravity = null;
});

function left(testid: string): number {
  return (page.getByTestId(testid).element() as HTMLElement).getBoundingClientRect().left;
}

test("missing CLI status and setup action stay on one line in their table columns", async () => {
  render(WelcomeScreenHost, { width: 576 });

  await expect.element(page.getByTestId("harness-setup-antigravity")).toBeVisible();
  await expect
    .poll(() => page.getByTestId("harness-install-antigravity").element().textContent)
    .toContain("Not installed");

  expect(
    Math.abs(left("harness-install-antigravity") - left("harness-install-claude_code")),
  ).toBeLessThanOrEqual(1);
  expect(
    Math.abs(left("harness-setup-antigravity") - left("harness-auth-claude_code")),
  ).toBeLessThanOrEqual(1);

  const install = page.getByTestId("harness-install-antigravity").element() as HTMLElement;
  const setup = page.getByTestId("harness-setup-antigravity").element() as HTMLElement;
  expect(install.getBoundingClientRect().height).toBeLessThan(24);
  expect(setup.getBoundingClientRect().height).toBeLessThan(30);

  const list = page.getByTestId("harness-status").element() as HTMLElement;
  expect(list.scrollWidth - list.clientWidth).toBeLessThanOrEqual(1);
});

test("a long installed version truncates without widening the table", async () => {
  browserState.versions.claude_code = `1.2.3-${"long-prerelease.".repeat(12)}`;
  render(WelcomeScreenHost, { width: 576 });

  await expect.element(page.getByTestId("harness-version-claude_code")).toBeVisible();
  const version = page.getByTestId("harness-version-claude_code").element() as HTMLElement;
  await expect.poll(() => version.scrollWidth - version.clientWidth).toBeGreaterThan(1);

  const list = page.getByTestId("harness-status").element() as HTMLElement;
  expect(list.scrollWidth - list.clientWidth).toBeLessThanOrEqual(1);
  await expect
    .element(page.getByTestId("harness-install-claude_code"))
    .toHaveTextContent("Installed");
});

test("narrow welcome layouts stack status and action without horizontal overflow", async () => {
  render(WelcomeScreenHost, { width: 340 });

  await expect.element(page.getByTestId("harness-setup-antigravity")).toBeVisible();

  const name = page.getByTestId("harness-label-antigravity").element() as HTMLElement;
  const install = page.getByTestId("harness-install-antigravity").element() as HTMLElement;
  const setup = page.getByTestId("harness-setup-antigravity").element() as HTMLElement;
  expect(
    Math.abs(install.getBoundingClientRect().left - name.getBoundingClientRect().left),
  ).toBeLessThanOrEqual(1);
  expect(
    Math.abs(setup.getBoundingClientRect().left - name.getBoundingClientRect().left),
  ).toBeLessThanOrEqual(1);
  expect(setup.getBoundingClientRect().top).toBeGreaterThanOrEqual(
    install.getBoundingClientRect().bottom,
  );
  expect(setup.getBoundingClientRect().height).toBeLessThan(30);

  const list = page.getByTestId("harness-status").element() as HTMLElement;
  expect(list.scrollWidth - list.clientWidth).toBeLessThanOrEqual(1);
});
