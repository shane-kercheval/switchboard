import { beforeEach, expect, test, vi } from "vitest";
import { page, userEvent } from "vitest/browser";
import { render } from "vitest-browser-svelte";
import SettingsHost from "./SettingsHost.svelte";
import { _testing as prefsTesting } from "$lib/preferences.svelte";
import { _testing as availabilityTesting } from "$lib/harnessAvailability.svelte";
import { SETTINGS_SECTIONS } from "$lib/settingsSections";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (command: string): Promise<unknown> => {
    if (command === "list_mcp_providers") return [];
    if (command === "mcp_status") {
      return { enabled: false, listening: false, port: 47821, url: null, token: null, error: null };
    }
    if (command === "get_harness_install_status") {
      return { installed: true, version: "1.0.0", path_source: "login_shell" };
    }
    if (command === "local_prompts_dir") return "/Users/test/Switchboard/prompts";
    if (command === "workflows_dir") return "/Users/test/Switchboard/workflows";
    if (command === "notification_availability") return "available";
    return null;
  }),
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => vi.fn()) }));
vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({ startDragging: vi.fn(async (): Promise<void> => undefined) }),
}));
vi.mock("$lib/native", () => ({ copyText: vi.fn(async (): Promise<void> => undefined) }));

beforeEach((): void => {
  prefsTesting.reset({ ready: true });
  availabilityTesting.reset();
});

for (const width of [760, 1100]) {
  test(`settings pages fit at ${width}px and scroll independently of navigation`, async (): Promise<void> => {
    await page.viewport(width, 650);
    render(SettingsHost, { width });
    const navigation = page.getByRole("navigation", { name: "Settings categories" });
    const initialTop = navigation.element().getBoundingClientRect().top;

    for (const section of SETTINGS_SECTIONS) {
      const button = navigation.getByRole("button", { name: section.label, exact: true });
      await button.click();
      await expect.element(button).toHaveAttribute("aria-current", "page");
      const panel = page.getByTestId(`settings-panel-${section.id}`);
      await expect.element(panel).toBeVisible();
      const element = panel.element() as HTMLElement;
      await expect.poll(() => element.scrollWidth - element.clientWidth).toBeLessThanOrEqual(1);
      for (const other of SETTINGS_SECTIONS.filter((item) => item.id !== section.id)) {
        expect(
          page.getByTestId(`settings-panel-${other.id}`).element().getBoundingClientRect().height,
        ).toBe(0);
      }
    }

    const accessButton = navigation.getByRole("button", { name: "Agent MCP Server", exact: true });
    (accessButton.element() as HTMLButtonElement).focus();
    await expect.element(accessButton).toHaveFocus();
    await userEvent.keyboard("{Enter}");
    await expect.element(accessButton).toHaveAttribute("aria-current", "page");
    const shortcutsButton = navigation.getByRole("button", {
      name: "Keyboard shortcuts",
      exact: true,
    });
    (shortcutsButton.element() as HTMLButtonElement).focus();
    await userEvent.keyboard(" ");
    await expect.element(shortcutsButton).toHaveAttribute("aria-current", "page");

    const shortcuts = page.getByTestId("settings-panel-shortcuts").element() as HTMLElement;
    await expect.poll(() => shortcuts.scrollHeight - shortcuts.clientHeight).toBeGreaterThan(100);
    shortcuts.scrollTop = shortcuts.scrollHeight;
    await expect.poll(() => shortcuts.scrollTop).toBeGreaterThan(100);
    expect(navigation.element().getBoundingClientRect().top).toBe(initialTop);
    await expect
      .element(navigation.getByRole("button", { name: "General", exact: true }))
      .toBeVisible();
    const host = page.getByTestId("settings-host").element() as HTMLElement;
    expect(host.scrollWidth - host.clientWidth).toBeLessThanOrEqual(1);
    expect(host.scrollHeight - host.clientHeight).toBeLessThanOrEqual(1);
  });
}
