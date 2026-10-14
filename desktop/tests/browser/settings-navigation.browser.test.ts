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

    await navigation.getByRole("button", { name: "Agents", exact: true }).click();
    const defaultSummary = page.getByTestId("agent-defaults-codex-summary");
    const defaultDetails = defaultSummary.element().closest("details")!;
    const chevron = defaultSummary.element().querySelector("svg")!;
    await expect.element(defaultSummary).toHaveTextContent("GPT-6.1 Sol · Default effort: Medium");
    expect(defaultDetails.open).toBe(false);
    const closedRotation = getComputedStyle(chevron).rotate;
    (defaultSummary.element() as HTMLElement).focus();
    await userEvent.keyboard("{Enter}");
    await expect.poll(() => defaultDetails.open).toBe(true);
    await expect.poll(() => getComputedStyle(chevron).rotate).not.toBe(closedRotation);
    await userEvent.keyboard(" ");
    await expect.poll(() => defaultDetails.open).toBe(false);
    await expect.poll(() => getComputedStyle(chevron).rotate).toBe(closedRotation);

    await navigation.getByRole("button", { name: "Prompts", exact: true }).click();
    const addServer = page.getByRole("button", { name: "Add prompt server", exact: true });
    const addForm = page.getByTestId("mcp-add-form");
    expect(addForm.element().getBoundingClientRect().height).toBe(0);
    (addServer.element() as HTMLButtonElement).focus();
    await userEvent.keyboard("{Enter}");
    await expect.element(page.getByRole("textbox", { name: "Name", exact: true })).toHaveFocus();
    const prompts = page.getByTestId("settings-panel-prompts").element() as HTMLElement;
    expect(prompts.scrollWidth - prompts.clientWidth).toBeLessThanOrEqual(1);
    await page.getByRole("button", { name: "Close setup", exact: true }).click();
    await expect.poll(() => addForm.element().getBoundingClientRect().height).toBe(0);
    await expect.element(addServer).toHaveFocus();

    await navigation.getByRole("button", { name: "Notifications", exact: true }).click();
    const help = page.getByRole("button", { name: "How reading mode works", exact: true });
    const details = document.getElementById(help.element().getAttribute("aria-controls")!)!;
    const notifications = page.getByTestId("settings-panel-notifications").element() as HTMLElement;
    const content = notifications.firstElementChild!;
    const collapsedHeight = content.getBoundingClientRect().height;
    expect(details.getBoundingClientRect().height).toBe(0);
    await expect.element(page.getByTestId("auto-reading-mode-toggle")).toBeVisible();
    (help.element() as HTMLButtonElement).focus();
    await userEvent.keyboard("{Enter}");
    await expect.element(help).toHaveAttribute("aria-expanded", "true");
    await expect.poll(() => details.getBoundingClientRect().height).toBeGreaterThan(0);
    expect(content.getBoundingClientRect().height).toBeGreaterThan(collapsedHeight);
    expect(navigation.element().getBoundingClientRect().top).toBe(initialTop);
    await navigation.getByRole("button", { name: "General", exact: true }).click();
    await navigation.getByRole("button", { name: "Notifications", exact: true }).click();
    await expect.element(help).toHaveAttribute("aria-expanded", "true");
    (help.element() as HTMLButtonElement).focus();
    await userEvent.keyboard(" ");
    await expect.poll(() => details.getBoundingClientRect().height).toBe(0);

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
