import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/svelte";
import { tick } from "svelte";
import SettingsView from "./SettingsView.svelte";
import { theme } from "$lib/theme.svelte";
import { agentCopy } from "$lib/agentCopy.svelte";
import { _testing as availabilityTesting } from "$lib/harnessAvailability.svelte";
import { _testing as prefsTesting } from "$lib/preferences.svelte";

// SettingsView embeds HarnessStatusList (probes install/auth on mount) and
// McpServersSettings (loads providers on mount). Tests that override the mock
// must keep these baseline stubs, so it's a named default restored per test.
const defaultInvoke = async (cmd: string, _args?: Record<string, unknown>): Promise<unknown> => {
  if (cmd === "get_harness_install_status") return { installed: true, version: "1.0.0" };
  if (cmd === "list_mcp_providers") return []; // embedded McpServersSettings loads on mount
  if (cmd === "local_prompts_dir")
    return "/Users/test/Library/Application Support/switchboard/prompts";
  return null; // auth probes resolve = authenticated
};
const invokeMock = vi.fn(defaultInvoke);
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
  // Restore the baseline impl so an override in one test can't leak into the next
  // (the embedded McpServersSettings loads on every mount).
  invokeMock.mockReset();
  invokeMock.mockImplementation(defaultInvoke);
  // The embedded HarnessStatusList reads the shared singleton store; reset it
  // so probed values don't leak across tests.
  availabilityTesting.reset();
  prefsTesting.reset();
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

  it("git-view editor preference defaults to code and persists edits", async () => {
    render(SettingsView, { props: { onClose: vi.fn() } });
    const editor = screen.getByTestId("git-editor-command") as HTMLInputElement;

    expect(editor.value).toBe("code");

    await fireEvent.input(editor, { target: { value: "cursor" } });
    await fireEvent.change(editor);
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("set_preferences", {
        preferences: {
          editor_command: "cursor",
          terminal_app: "Terminal",
          diff_style: "unified",
        },
      }),
    );

    // Clearing the field persists null (fall back to OS default), not "".
    invokeMock.mockClear();
    await fireEvent.input(editor, { target: { value: "  " } });
    await fireEvent.change(editor);
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("set_preferences", {
        preferences: { editor_command: null, terminal_app: "Terminal", diff_style: "unified" },
      }),
    );
  });

  it("git-view terminal preference persists, defaulting a blank to Terminal", async () => {
    render(SettingsView, { props: { onClose: vi.fn() } });
    const terminal = screen.getByTestId("git-terminal-app");

    await fireEvent.input(terminal, { target: { value: "iTerm" } });
    await fireEvent.change(terminal);
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("set_preferences", {
        preferences: { editor_command: "code", terminal_app: "iTerm", diff_style: "unified" },
      }),
    );

    invokeMock.mockClear();
    await fireEvent.input(terminal, { target: { value: "" } });
    await fireEvent.change(terminal);
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("set_preferences", {
        preferences: { editor_command: "code", terminal_app: "Terminal", diff_style: "unified" },
      }),
    );
  });

  it("surfaces an inline error when a preference save fails, keeping the value", async () => {
    // A failed config.yaml write must not be silent: the user sees an error and
    // the typed value stays (surface-and-keep, not revert).
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "get_harness_install_status") return { installed: true, version: "1.0.0" };
      if (cmd === "list_mcp_providers") return [];
      if (cmd === "set_preferences") throw new Error("disk full");
      return null;
    });
    render(SettingsView, { props: { onClose: vi.fn() } });
    const editor = screen.getByTestId("git-editor-command") as HTMLInputElement;

    await fireEvent.input(editor, { target: { value: "cursor" } });
    await fireEvent.change(editor);

    await waitFor(() => expect(screen.getByTestId("git-prefs-save-error")).toBeInTheDocument());
    // Value is kept, not reverted.
    expect(editor.value).toBe("cursor");
  });

  it("shortcuts section lists expected keyboard shortcuts", () => {
    render(SettingsView, { props: { onClose: vi.fn() } });
    expect(screen.getByText("Focus message box")).toBeInTheDocument();
    expect(screen.getByText("Jump to next unread project")).toBeInTheDocument();
    expect(screen.getByText("Show current project in Git view")).toBeInTheDocument();
    expect(screen.getByText("Open selection in editor")).toBeInTheDocument();
    expect(screen.getByText("Expand or restore Git details panel")).toBeInTheDocument();
    expect(screen.getByText("Toggle projects sidebar")).toBeInTheDocument();
    expect(screen.getByText("Toggle agents sidebar")).toBeInTheDocument();
    expect(screen.getByText("Toggle settings")).toBeInTheDocument();
  });

  it("shows the local prompts folder and opens it in Finder", async () => {
    render(SettingsView, { props: { onClose: vi.fn() } });

    await waitFor(() =>
      expect(screen.getByTestId("local-prompts-dir")).toHaveTextContent(
        "/Users/test/Library/Application Support/switchboard/prompts",
      ),
    );

    await fireEvent.click(screen.getByTestId("local-prompts-open"));

    expect(invokeMock).toHaveBeenCalledWith("open_local_prompts_dir", undefined);
  });
});
