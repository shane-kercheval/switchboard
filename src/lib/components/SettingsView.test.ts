import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/svelte";
import { tick } from "svelte";
import SettingsView from "./SettingsView.svelte";
import { theme } from "$lib/theme.svelte";
import { agentCopy } from "$lib/agentCopy.svelte";
import { _testing as availabilityTesting } from "$lib/harnessAvailability.svelte";
import { _testing as prefsTesting } from "$lib/preferences.svelte";
import { WORKFLOW_AUTHORING_GUIDE_URL } from "$lib/workflowAuthoring";
import { DEFAULT_AGENT_PROFILES } from "$lib/agentSelection";
import type { Preferences } from "$lib/types";

// SettingsView embeds HarnessStatusList (probes install/auth on mount) and
// McpServersSettings (loads providers on mount). Tests that override the mock
// must keep these baseline stubs, so it's a named default restored per test.
const defaultInvoke = async (cmd: string, _args?: Record<string, unknown>): Promise<unknown> => {
  if (cmd === "get_harness_install_status")
    return { installed: true, version: "1.0.0", path_source: "login_shell" };
  if (cmd === "list_mcp_providers") return []; // embedded McpServersSettings loads on mount
  if (cmd === "local_prompts_dir")
    return "/Users/test/Library/Application Support/switchboard/prompts";
  if (cmd === "workflows_dir")
    return "/Users/test/Library/Application Support/switchboard/workflows";
  if (cmd === "get_preferences")
    return {
      editor_command: "code",
      terminal_app: "Terminal",
      diff_style: "unified",
      show_builtins: true,
      notify_on_completion: true,
      notify_while_focused: false,
    };
  if (cmd === "notification_availability") return "available";
  return null; // auth probes resolve = authenticated
};
const invokeMock = vi.fn(defaultInvoke);
const copyTextMock = vi.fn(async (_text: string): Promise<void> => undefined);
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: Record<string, unknown>) => invokeMock(cmd, args),
}));
vi.mock("$lib/native", () => ({
  copyText: (text: string) => copyTextMock(text),
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
  copyTextMock.mockClear();
  // The embedded HarnessStatusList reads the shared singleton store; reset it
  // so probed values don't leak across tests.
  availabilityTesting.reset();
  prefsTesting.reset({ ready: true });
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

  it("persists per-harness primary and optional secondary defaults", async () => {
    render(SettingsView, { props: { onClose: vi.fn() } });
    await fireEvent.click(await screen.findByText("Codex", { selector: "summary" }));
    await fireEvent.click(
      screen.getByTestId("settings-profile-codex-primary-model-option-gpt-5.6-terra"),
    );
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("set_preferences", {
        preferences: expect.objectContaining({
          agent_defaults: expect.objectContaining({
            codex: expect.objectContaining({
              primary: { model: "gpt-5.6-terra", effort: "high" },
            }),
          }),
        }),
      }),
    );

    await fireEvent.click(screen.getByTestId("settings-profile-codex-secondary-toggle"));
    expect(screen.getByTestId("settings-profile-codex-secondary-model")).toBeInTheDocument();
    await waitFor(() =>
      expect(invokeMock).toHaveBeenLastCalledWith("set_preferences", {
        preferences: expect.objectContaining({
          agent_defaults: expect.objectContaining({
            codex: {
              primary: { model: "gpt-5.6-terra", effort: "high" },
              secondary: { model: "gpt-5.6-terra", effort: "medium" },
            },
          }),
        }),
      }),
    );
  });

  it("keeps backend preference controls unavailable until saved values are authoritative", async () => {
    prefsTesting.reset({ ready: false });
    let resolvePreferences!: (value: Preferences) => void;
    const delayedPreferences = new Promise<Preferences>((resolve) => {
      resolvePreferences = resolve;
    });
    invokeMock.mockImplementation((cmd: string, args?: Record<string, unknown>) => {
      if (cmd === "get_preferences") return delayedPreferences;
      return defaultInvoke(cmd, args);
    });

    render(SettingsView, { props: { onClose: vi.fn() } });
    expect(screen.getByTestId("agent-defaults-loading")).toBeInTheDocument();
    expect(screen.queryByTestId("settings-profile-claude_code-primary-model")).toBeNull();
    expect(screen.getByTestId("external-editor-command")).toBeDisabled();
    expect(screen.getByTestId("external-terminal-app")).toHaveAttribute("aria-disabled", "true");
    expect(screen.getByTestId("external-terminal-app-option-terminal")).toBeDisabled();
    expect(screen.getByTestId("external-terminal-app-option-iterm")).toBeDisabled();
    expect(screen.getByTestId("notify-toggle")).toBeDisabled();
    expect(screen.getByTestId("notify-while-focused-toggle")).toBeDisabled();
    expect(screen.getByTestId("show-builtins-toggle")).toBeDisabled();

    resolvePreferences({
      editor_command: "zed",
      terminal_app: "iTerm",
      diff_style: "unified",
      show_builtins: true,
      notify_on_completion: true,
      notify_while_focused: false,
      agent_defaults: {
        ...structuredClone(DEFAULT_AGENT_PROFILES),
        claude_code: {
          primary: { model: "sonnet", effort: "low" },
          secondary: null,
        },
      },
    });

    await waitFor(() =>
      expect(screen.getByTestId("settings-profile-claude_code-primary-model")).toHaveAttribute(
        "data-value",
        "sonnet",
      ),
    );
    const editor = screen.getByTestId("external-editor-command") as HTMLInputElement;
    const terminal = screen.getByTestId("external-terminal-app");
    expect(editor).toBeEnabled();
    expect(terminal).toHaveAttribute("aria-disabled", "false");
    expect(editor).toHaveValue("zed");
    expect(terminal).toHaveAttribute("data-value", "iTerm");
    expect(screen.getByTestId("notify-toggle")).toBeEnabled();
    expect(screen.getByTestId("show-builtins-toggle")).toBeEnabled();

    await fireEvent.input(editor, { target: { value: "cursor" } });
    await fireEvent.change(editor);
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("set_preferences", {
        preferences: expect.objectContaining({
          editor_command: "cursor",
          terminal_app: "iTerm",
        }),
      }),
    );

    await fireEvent.click(
      screen.getByTestId("settings-profile-claude_code-primary-effort-option-medium"),
    );
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("set_preferences", {
        preferences: expect.objectContaining({
          agent_defaults: expect.objectContaining({
            claude_code: {
              primary: { model: "sonnet", effort: "medium" },
              secondary: null,
            },
          }),
        }),
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

  it("external editor preference defaults to code and persists edits", async () => {
    render(SettingsView, { props: { onClose: vi.fn() } });
    const editor = screen.getByTestId("external-editor-command") as HTMLInputElement;

    expect(editor.value).toBe("code");

    await fireEvent.input(editor, { target: { value: "cursor" } });
    await fireEvent.change(editor);
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("set_preferences", {
        preferences: {
          editor_command: "cursor",
          terminal_app: "Terminal",
          diff_style: "unified",
          show_builtins: true,
          notify_on_completion: true,
          notify_while_focused: false,
          agent_defaults: expect.any(Object),
        },
      }),
    );

    // Clearing the field persists null (fall back to OS default), not "".
    invokeMock.mockClear();
    await fireEvent.input(editor, { target: { value: "  " } });
    await fireEvent.change(editor);
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("set_preferences", {
        preferences: {
          editor_command: null,
          terminal_app: "Terminal",
          diff_style: "unified",
          show_builtins: true,
          notify_on_completion: true,
          notify_while_focused: false,
          agent_defaults: expect.any(Object),
        },
      }),
    );
  });

  it("selects Terminal or iTerm for external terminal actions", async () => {
    render(SettingsView, { props: { onClose: vi.fn() } });
    const terminal = screen.getByTestId("external-terminal-app");

    expect(terminal).toHaveAttribute("data-value", "Terminal");
    await fireEvent.click(screen.getByTestId("external-terminal-app-option-iterm"));
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("set_preferences", {
        preferences: {
          editor_command: "code",
          terminal_app: "iTerm",
          diff_style: "unified",
          show_builtins: true,
          notify_on_completion: true,
          notify_while_focused: false,
          agent_defaults: expect.any(Object),
        },
      }),
    );

    invokeMock.mockClear();
    await fireEvent.click(screen.getByTestId("external-terminal-app-option-terminal"));
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("set_preferences", {
        preferences: {
          editor_command: "code",
          terminal_app: "Terminal",
          diff_style: "unified",
          show_builtins: true,
          notify_on_completion: true,
          notify_while_focused: false,
          agent_defaults: expect.any(Object),
        },
      }),
    );
  });

  it("toggles show_builtins and persists the change", async () => {
    render(SettingsView, { props: { onClose: vi.fn() } });
    const toggle = screen.getByTestId("show-builtins-toggle");
    // Defaults on.
    expect(toggle).toHaveAttribute("aria-checked", "true");

    await fireEvent.click(toggle);
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("set_preferences", {
        preferences: expect.objectContaining({ show_builtins: false }),
      }),
    );
    expect(toggle).toHaveAttribute("aria-checked", "false");
  });

  it("surfaces an inline error when a preference save fails, keeping the value", async () => {
    // A failed config.yaml write must not be silent: the user sees an error and
    // the typed value stays (surface-and-keep, not revert).
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "get_harness_install_status")
        return { installed: true, version: "1.0.0", path_source: "login_shell" };
      if (cmd === "list_mcp_providers") return [];
      if (cmd === "set_preferences") throw new Error("disk full");
      return null;
    });
    render(SettingsView, { props: { onClose: vi.fn() } });
    const editor = screen.getByTestId("external-editor-command") as HTMLInputElement;

    await fireEvent.input(editor, { target: { value: "cursor" } });
    await fireEvent.change(editor);

    await waitFor(() => expect(screen.getByTestId("external-apps-save-error")).toBeInTheDocument());
    // Value is kept, not reverted.
    expect(editor.value).toBe("cursor");
  });

  it("shortcuts section lists expected keyboard shortcuts", () => {
    render(SettingsView, { props: { onClose: vi.fn() } });
    // The command palette is listed first.
    const actions = screen.getAllByTestId("shortcut-action");
    expect(actions[0]).toHaveTextContent("Open command palette");
    expect(screen.getByText("Focus message box")).toBeInTheDocument();
    expect(screen.getByText("Add project")).toBeInTheDocument();
    expect(screen.getByText("Add repository")).toBeInTheDocument();
    expect(screen.getByText("Refresh all repositories")).toBeInTheDocument();
    expect(screen.getByText("Jump to next unread project")).toBeInTheDocument();
    expect(screen.getByText("Show current project in Git view")).toBeInTheDocument();
    expect(screen.getByText("Open selection in editor")).toBeInTheDocument();
    expect(screen.getByText("Expand or restore Git details panel")).toBeInTheDocument();
    expect(screen.getByText("Toggle projects sidebar")).toBeInTheDocument();
    expect(screen.getByText("Toggle agents sidebar")).toBeInTheDocument();
    expect(screen.getByText("Toggle Agents / Pins sidebar")).toBeInTheDocument();
    expect(screen.getByText("Toggle settings")).toBeInTheDocument();
    expect(screen.getByText("Cycle to previous / next pane")).toBeInTheDocument();
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

  it("shows the user-global workflows folder and opens it in Finder", async () => {
    render(SettingsView, { props: { onClose: vi.fn() } });

    await waitFor(() =>
      expect(screen.getByTestId("workflows-dir")).toHaveTextContent(
        "/Users/test/Library/Application Support/switchboard/workflows",
      ),
    );

    await fireEvent.click(screen.getByTestId("workflows-open"));

    expect(invokeMock).toHaveBeenCalledWith("open_workflows_dir", undefined);
  });

  it("previews and copies the workflow authoring prompt", async () => {
    render(SettingsView, { props: { onClose: vi.fn() } });

    const toggle = screen.getByTestId("workflow-authoring-toggle");
    expect(toggle).toHaveAttribute("aria-expanded", "false");
    expect(screen.queryByTestId("workflow-authoring-prompt")).not.toBeInTheDocument();

    await waitFor(() => expect(screen.getByTestId("workflows-dir")).toBeInTheDocument());
    await fireEvent.click(screen.getByTestId("workflow-authoring-copy"));
    await waitFor(() => expect(copyTextMock).toHaveBeenCalledOnce());

    const copied = copyTextMock.mock.calls[0]![0];
    expect(copied).toContain(WORKFLOW_AUTHORING_GUIDE_URL);
    expect(copied).toContain("/Users/test/Library/Application Support/switchboard/workflows");

    await fireEvent.click(toggle);
    expect(toggle).toHaveAttribute("aria-expanded", "true");
    expect(screen.getByTestId("workflow-authoring-prompt").textContent).toBe(copied);
  });
});

describe("SettingsView — notifications", () => {
  /// Override one command on top of the baseline stubs, which the embedded
  /// HarnessStatusList / McpServersSettings still need.
  const withCommand = (cmd: string, value: unknown): void => {
    invokeMock.mockImplementation(async (c: string, args?: Record<string, unknown>) =>
      c === cmd ? value : defaultInvoke(c, args),
    );
  };

  it("the background-projects toggle is off by default and persists a flip", async () => {
    render(SettingsView, { props: { onClose: vi.fn() } });
    const toggle = await screen.findByTestId("notify-while-focused-toggle");
    await waitFor(() => expect(toggle).toHaveAttribute("aria-checked", "false"));

    await fireEvent.click(toggle);
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith(
        "set_preferences",
        expect.objectContaining({
          preferences: expect.objectContaining({ notify_while_focused: true }),
        }),
      ),
    );
    expect(toggle).toHaveAttribute("aria-checked", "true");
  });

  it("disables the background-projects toggle when notifications are off entirely", async () => {
    // It is meaningless on its own — without the master switch there is nothing
    // to route. Leaving it live would offer a choice that does nothing.
    render(SettingsView, { props: { onClose: vi.fn() } });
    const master = await screen.findByTestId("notify-toggle");
    const nested = screen.getByTestId("notify-while-focused-toggle");
    expect(nested).not.toBeDisabled();

    await fireEvent.click(master);
    await waitFor(() => expect(nested).toBeDisabled());
  });

  it("explains the two rules a user cannot infer from the toggle", async () => {
    // Both are load-bearing: without the first, testing the toggle while looking
    // at the app reads as broken; without the second, "sound but no banner" looks
    // impossible when it is actually a macOS setting.
    render(SettingsView, { props: { onClose: vi.fn() } });
    const section = screen.getByTestId("notification-prefs");
    expect(within(section).getByText(/the project on screen never notifies/i)).toBeInTheDocument();
    expect(within(section).getByText(/System Settings → Notifications/i)).toBeInTheDocument();
  });

  it("toggle reflects the stored preference and persists a flip", async () => {
    render(SettingsView, { props: { onClose: vi.fn() } });
    const toggle = await screen.findByTestId("notify-toggle");
    await waitFor(() => expect(toggle).toHaveAttribute("aria-checked", "true"));

    await fireEvent.click(toggle);
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith(
        "set_preferences",
        expect.objectContaining({
          preferences: expect.objectContaining({ notify_on_completion: false }),
        }),
      ),
    );
    expect(toggle).toHaveAttribute("aria-checked", "false");
  });

  it("warns only when macOS is actually suppressing notifications", async () => {
    withCommand("notification_availability", "suppressed");
    render(SettingsView, { props: { onClose: vi.fn() } });
    expect(await screen.findByTestId("notify-suppressed")).toBeInTheDocument();
  });

  it("stays quiet when notifications are available", async () => {
    // The sound-only configuration (alerts off, sound on) classifies as available
    // on the backend, so this is also the assertion that a working setup is not
    // reported as broken.
    render(SettingsView, { props: { onClose: vi.fn() } });
    await screen.findByTestId("notify-toggle");
    expect(screen.queryByTestId("notify-suppressed")).not.toBeInTheDocument();
  });

  it("stays quiet in an unbundled dev build rather than blaming the user's settings", async () => {
    // `unavailable` is not `suppressed`: nothing is misconfigured, the build just
    // isn't an installed app. Showing the blocking warning here would send a
    // developer to System Settings for no reason.
    withCommand("notification_availability", "unavailable");
    render(SettingsView, { props: { onClose: vi.fn() } });
    await screen.findByTestId("notify-toggle");
    expect(screen.queryByTestId("notify-suppressed")).not.toBeInTheDocument();
  });

  it("renders no hint when the availability probe itself fails", async () => {
    invokeMock.mockImplementation(async (c: string, args?: Record<string, unknown>) =>
      c === "notification_availability"
        ? Promise.reject(new Error("boom"))
        : defaultInvoke(c, args),
    );
    render(SettingsView, { props: { onClose: vi.fn() } });
    await screen.findByTestId("notify-toggle");
    expect(screen.queryByTestId("notify-suppressed")).not.toBeInTheDocument();
  });
});

describe("SettingsView — preference save failures", () => {
  /// Reject only `set_preferences`, keeping the baseline stubs the embedded
  /// components need on mount.
  const failSaves = (): void => {
    invokeMock.mockImplementation(async (c: string, args?: Record<string, unknown>) =>
      c === "set_preferences" ? Promise.reject(new Error("disk full")) : defaultInvoke(c, args),
    );
  };

  it("reports a failed notification save beside the notification toggles", async () => {
    // Previously this surfaced under Git View — plausibly scrolled off screen —
    // so a user would see the toggle flip with no error and assume it stuck.
    failSaves();
    render(SettingsView, { props: { onClose: vi.fn() } });
    await fireEvent.click(await screen.findByTestId("notify-toggle"));

    expect(await screen.findByTestId("notify-save-error")).toBeInTheDocument();
  });

  it("reports a failed external-app preference save beside those controls", async () => {
    failSaves();
    render(SettingsView, { props: { onClose: vi.fn() } });
    const editor = screen.getByTestId("external-editor-command") as HTMLInputElement;
    await fireEvent.input(editor, { target: { value: "cursor" } });
    await fireEvent.change(editor);

    expect(await screen.findByTestId("external-apps-save-error")).toBeInTheDocument();
    expect(screen.queryByTestId("notify-save-error")).not.toBeInTheDocument();
  });

  it("a later successful save clears an earlier failure, including for other keys", async () => {
    // Not sloppy attribution — accurate. Every write sends the whole merged
    // object and memory is updated optimistically first, so the Git save below
    // carries the failed notification value with it and does persist it.
    failSaves();
    render(SettingsView, { props: { onClose: vi.fn() } });
    await fireEvent.click(await screen.findByTestId("notify-toggle"));
    expect(await screen.findByTestId("notify-save-error")).toBeInTheDocument();

    invokeMock.mockImplementation(defaultInvoke);
    const editor = screen.getByTestId("external-editor-command") as HTMLInputElement;
    await fireEvent.input(editor, { target: { value: "cursor" } });
    await fireEvent.change(editor);

    await waitFor(() => expect(screen.queryByTestId("notify-save-error")).not.toBeInTheDocument());
  });
});
