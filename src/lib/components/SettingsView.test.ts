import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/svelte";
import { tick } from "svelte";
import SettingsView from "./SettingsView.svelte";
import { theme } from "$lib/theme.svelte";
import { agentCopy } from "$lib/agentCopy.svelte";
import { _testing as availabilityTesting } from "$lib/harnessAvailability.svelte";

// SettingsView embeds HarnessStatusList, which probes install/auth on mount.
const invokeMock = vi.fn(async (cmd: string, _args?: Record<string, unknown>) => {
  if (cmd === "get_harness_install_status") return { installed: true, version: "1.0.0" };
  if (cmd === "list_mcp_providers") return []; // embedded McpServersSettings loads on mount
  return null; // auth probes resolve = authenticated
});
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: Record<string, unknown>) => invokeMock(cmd, args),
}));
// Embedded McpServersSettings subscribes to `prompts:synced` on mount.
vi.mock("@tauri-apps/api/event", () => ({
  listen: () => Promise.resolve(() => {}),
}));

beforeEach(() => {
  theme.set("system");
  agentCopy.set("last_answer_block");
  invokeMock.mockClear();
  // The embedded HarnessStatusList reads the shared singleton store; reset it
  // so probed values don't leak across tests.
  availabilityTesting.reset();
});

afterEach(() => {
  document.documentElement.classList.remove("dark");
});

describe("SettingsView", () => {
  it("close button fires onClose", async () => {
    const onClose = vi.fn();
    render(SettingsView, { props: { onClose } });
    await fireEvent.click(screen.getByTestId("settings-close"));
    expect(onClose).toHaveBeenCalledOnce();
  });

  it("renders a Supported CLIs section with the harness status list", async () => {
    render(SettingsView, { props: { onClose: vi.fn() } });
    expect(screen.getByText("Supported CLIs")).toBeInTheDocument();
    await waitFor(() => expect(screen.getByTestId("harness-status")).toBeInTheDocument());
    // The shared list probed install status for each harness.
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_harness_install_status", {
        harness: "claude_code",
      }),
    );
  });

  it("theme picker has role=radiogroup and each option has role=radio", () => {
    render(SettingsView, { props: { onClose: vi.fn() } });
    const group = screen.getByRole("radiogroup", { name: "Theme" });
    expect(group).toBeInTheDocument();
    const radios = within(group).getAllByRole("radio");
    expect(radios).toHaveLength(3);
    const labels = radios.map((r) => r.textContent?.trim());
    expect(labels).toEqual(["System", "Light", "Dark"]);
  });

  it("aria-checked tracks the active theme and updates on click", async () => {
    render(SettingsView, { props: { onClose: vi.fn() } });
    const group = screen.getByRole("radiogroup", { name: "Theme" });
    const [system, light, dark] = within(group).getAllByRole("radio");

    // Initial state: system is checked
    expect(system).toHaveAttribute("aria-checked", "true");
    expect(light).toHaveAttribute("aria-checked", "false");
    expect(dark).toHaveAttribute("aria-checked", "false");

    await fireEvent.click(light!);
    await tick();
    expect(system).toHaveAttribute("aria-checked", "false");
    expect(light).toHaveAttribute("aria-checked", "true");
    expect(dark).toHaveAttribute("aria-checked", "false");

    await fireEvent.click(dark!);
    await tick();
    expect(dark).toHaveAttribute("aria-checked", "true");
    expect(light).toHaveAttribute("aria-checked", "false");
  });

  it("agent message copy picker updates the copy preference", async () => {
    render(SettingsView, { props: { onClose: vi.fn() } });
    const group = screen.getByRole("radiogroup", { name: "Agent message copy" });
    const [lastBlock, fullAnswer] = within(group).getAllByRole("radio");

    expect(lastBlock).toHaveAttribute("aria-checked", "true");
    expect(fullAnswer).toHaveAttribute("aria-checked", "false");

    await fireEvent.click(fullAnswer!);
    await tick();

    expect(agentCopy.mode).toBe("full_answer");
    expect(lastBlock).toHaveAttribute("aria-checked", "false");
    expect(fullAnswer).toHaveAttribute("aria-checked", "true");
  });

  it("shortcuts section lists expected keyboard shortcuts", () => {
    render(SettingsView, { props: { onClose: vi.fn() } });
    expect(screen.getByText("Toggle projects sidebar")).toBeInTheDocument();
    expect(screen.getByText("Toggle agents sidebar")).toBeInTheDocument();
    expect(screen.getByText("Toggle settings")).toBeInTheDocument();
  });
});
