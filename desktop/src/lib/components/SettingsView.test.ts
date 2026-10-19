import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/svelte";
import { tick } from "svelte";
import SettingsView from "./SettingsView.svelte";
import { theme } from "$lib/theme.svelte";
import { agentCopy } from "$lib/agentCopy.svelte";
import { _testing as availabilityTesting } from "$lib/harnessAvailability.svelte";
import { _testing as prefsTesting, preferences } from "$lib/preferences.svelte";
import { WORKFLOW_AUTHORING_GUIDE_URL } from "$lib/workflowAuthoring";
import { DATA_SCIENCE_ORCHESTRATOR_PROMPT, ORCHESTRATOR_PROMPT } from "$lib/orchestratorPrompt";
import { DEFAULT_AGENT_SELECTIONS } from "$lib/agentSelection";
import type { Preferences } from "$lib/types";

// No model agy offers today lacks an effort axis, but the pickers still handle
// one (agy 1.1.19 offered two), so these tests add one to the model table.
vi.mock("$lib/antigravityModels.json", async (importOriginal) =>
  (await import("../../../tests/fixtures/antigravityModels")).withStandIns(importOriginal),
);

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
      show_builtin_prompts: true,
      show_builtin_workflows: true,
      claude_chrome_enabled: false,
      auto_reading_mode: false,
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

  it("opens the prompts page when requested", async (): Promise<void> => {
    render(SettingsView, { props: { onClose: vi.fn(), section: "prompts" } });
    expect(screen.getByRole("heading", { level: 1 })).toHaveTextContent("Prompts");
    expect(screen.getByTestId("prompt-settings-section")).toBeVisible();
    expect(screen.getByTestId("settings-panel-general")).not.toBeVisible();
  });

  it("shows only the selected category and preserves unfinished prompt input", async (): Promise<void> => {
    const view = render(SettingsView, { props: { onClose: vi.fn(), section: "prompts" } });
    await fireEvent.click(screen.getByRole("button", { name: "Add prompt server" }));
    const name = await screen.findByRole("textbox", { name: "Name" });
    await fireEvent.input(name, { target: { value: "My prompt server" } });
    await view.rerender({ section: "general" });
    expect(screen.getByRole("heading", { level: 1 })).toHaveTextContent("General");
    expect(screen.queryByRole("textbox", { name: "Name" })).not.toBeInTheDocument();
    expect(screen.getByTestId("auto-reading-mode-toggle")).not.toBeVisible();
    await view.rerender({ section: "notifications" });
    expect(screen.getByTestId("auto-reading-mode-toggle")).toBeVisible();
    expect(screen.getByTestId("notify-toggle")).toBeVisible();
    await view.rerender({ section: "workflows" });
    expect(screen.getByRole("heading", { level: 1 })).toHaveTextContent("Workflows");
    expect(screen.getByTestId("workflows-open")).toBeVisible();
    expect(screen.getByTestId("prompt-settings-section")).not.toBeVisible();
    expect(screen.getByTestId("notify-toggle")).not.toBeVisible();
    await view.rerender({ section: "prompts" });
    expect(screen.getByRole("textbox", { name: "Name" })).toHaveValue("My prompt server");
    expect(screen.getByTestId("workflows-open")).not.toBeVisible();
  });

  it("renders a Supported CLIs section with the harness status list", async () => {
    render(SettingsView, { props: { onClose: vi.fn(), section: "agents" } });
    expect(screen.getByText("Supported CLIs")).toBeInTheDocument();
    await waitFor(() => expect(screen.getByTestId("harness-status")).toBeInTheDocument());
    // The shared list probed install status for each harness.
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_harness_install_status", {
        harness: "claude_code",
      }),
    );
  });

  it("persists independent quick choices and explicit defaults atomically", async () => {
    render(SettingsView, { props: { onClose: vi.fn(), section: "agents" } });
    await fireEvent.click(await screen.findByTestId("agent-defaults-codex-summary"));
    await fireEvent.click(screen.getByTestId("settings-selection-codex-model-choice-gpt-6-astra"));
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("set_preferences", {
        preferences: expect.objectContaining({
          agent_defaults: expect.objectContaining({
            codex: {
              model_choices: ["gpt-6.1-sol", "gpt-5.6-terra", "gpt-6-astra"],
              effort_choices: ["medium", "high"],
              default_model: "gpt-6.1-sol",
              default_effort: "medium",
            },
          }),
        }),
      }),
    );

    expect(screen.getByTestId("agent-defaults-codex-summary")).toHaveTextContent("GPT-6.1 Sol");
    await fireEvent.change(screen.getByTestId("settings-selection-codex-model-current"), {
      target: { value: "gpt-5.6-terra" },
    });
    await waitFor(() =>
      expect(invokeMock).toHaveBeenLastCalledWith("set_preferences", {
        preferences: expect.objectContaining({
          agent_defaults: expect.objectContaining({
            codex: {
              model_choices: ["gpt-6.1-sol", "gpt-5.6-terra", "gpt-6-astra"],
              effort_choices: ["medium", "high"],
              default_model: "gpt-5.6-terra",
              default_effort: "medium",
            },
          }),
        }),
      }),
    );
    await waitFor(() =>
      expect(screen.getByTestId("agent-defaults-codex-summary")).toHaveTextContent("GPT-5.6 Terra"),
    );
  });

  // Leaving the Agents section hides it rather than unmounting it, so a card a
  // person opened is still open when they come back without closing Settings.
  it("keeps an opened harness card open across leaving and returning to the Agents section", async () => {
    const view = render(SettingsView, { props: { onClose: vi.fn(), section: "agents" } });
    const card = screen.getByTestId("agent-defaults-codex-summary").closest("details");
    const toggled = new Promise((resolve) =>
      card?.addEventListener("toggle", resolve, { once: true }),
    );
    await fireEvent.click(screen.getByTestId("agent-defaults-codex-summary"));
    await toggled;

    await view.rerender({ section: "prompts" });
    await view.rerender({ section: "agents" });

    expect(screen.getByTestId("agent-defaults-codex-summary").closest("details")?.open).toBe(true);
    expect(screen.getByTestId("agent-defaults-claude_code-summary").closest("details")?.open).toBe(
      true,
    );
  });

  // Saving an unrelated preference must neither collapse nor reopen a card.
  it("keeps the harness cards as the person left them when an unrelated preference is saved", async () => {
    render(SettingsView, { props: { onClose: vi.fn(), section: "agents" } });
    const card = (harness: string) =>
      screen.getByTestId(`agent-defaults-${harness}-summary`).closest("details");
    const toggle = async (harness: string) => {
      const toggled = new Promise((resolve) =>
        card(harness)?.addEventListener("toggle", resolve, { once: true }),
      );
      await fireEvent.click(screen.getByTestId(`agent-defaults-${harness}-summary`));
      await toggled;
    };
    await toggle("claude_code");
    await toggle("codex");

    await fireEvent.click(screen.getByTestId("claude-chrome-toggle"));
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("set_preferences", {
        preferences: expect.objectContaining({ claude_chrome_enabled: true }),
      }),
    );

    expect(card("claude_code")?.open).toBe(false);
    expect(card("codex")?.open).toBe(true);
  });

  it("shows custom model and effort names and unset defaults in the headers", (): void => {
    preferences.agent_defaults.codex.default_model = "custom-model";
    preferences.agent_defaults.codex.default_effort = "custom-effort";
    preferences.agent_defaults.claude_code.default_model = null;
    preferences.agent_defaults.claude_code.default_effort = null;
    render(SettingsView, { props: { onClose: vi.fn(), section: "agents" } });
    expect(screen.getByTestId("agent-defaults-codex-summary")).toHaveTextContent(
      "Default model: custom-model · Default effort: custom-effort",
    );
    expect(screen.getByTestId("agent-defaults-claude_code-summary")).toHaveTextContent(
      "Default model: Not set · Default effort: Not set",
    );
  });

  it("falls back to the first remaining default when the current choice is removed", async () => {
    preferences.agent_defaults.claude_code = {
      model_choices: ["fable", "opus", "sonnet"],
      effort_choices: ["high", "medium"],
      default_model: "fable",
      default_effort: "high",
    };
    render(SettingsView, { props: { onClose: vi.fn(), section: "agents" } });

    await fireEvent.click(screen.getByTestId("settings-selection-claude_code-model-choice-fable"));
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("set_preferences", {
        preferences: expect.objectContaining({
          agent_defaults: expect.objectContaining({
            claude_code: {
              model_choices: ["opus", "sonnet"],
              effort_choices: ["high", "medium"],
              default_model: "opus",
              default_effort: "high",
            },
          }),
        }),
      }),
    );

    expect(screen.getByTestId("agent-defaults-claude_code-summary")).toHaveTextContent(
      "Default effort: High",
    );
    await fireEvent.click(screen.getByTestId("settings-selection-claude_code-effort-choice-high"));
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("set_preferences", {
        preferences: expect.objectContaining({
          agent_defaults: expect.objectContaining({
            claude_code: {
              model_choices: ["opus", "sonnet"],
              effort_choices: ["medium"],
              default_model: "opus",
              default_effort: "medium",
            },
          }),
        }),
      }),
    );
    await waitFor(() =>
      expect(screen.getByTestId("agent-defaults-claude_code-summary")).toHaveTextContent(
        "Default effort: Medium",
      ),
    );
  });

  it("lists every harness in Agent Defaults", () => {
    render(SettingsView, { props: { onClose: vi.fn(), section: "agents" } });
    const section = screen.getByTestId("agent-defaults-settings");
    expect(section).toHaveTextContent(
      "Choose which models and reasoning efforts are available for quick switching from the Agents sidebar, and which ones new agents start with. New projects use the same defaults for their initial agents.",
    );
    const labels = Array.from(section.querySelectorAll("summary")).map((el) =>
      el.textContent?.replace(/\s+/g, " ").trim(),
    );
    expect(labels).toEqual([
      "Claude Default model: Opus · Default effort: Medium",
      "Codex Default model: GPT-6.1 Sol · Default effort: Medium",
      "Antigravity Default model: Gemini 3.8 Flash · Default effort: Medium",
    ]);
  });

  // Only an explicit expand or collapse control may resize the page: changing
  // a default inside an open harness card must leave the card open, and the
  // same element (a rebuilt card would replace the dropdown the person just
  // used, taking its keyboard focus).
  it("keeps a harness's defaults card open when its default model or effort changes", async () => {
    render(SettingsView, { props: { onClose: vi.fn(), section: "agents" } });
    const summary = screen.getByTestId("agent-defaults-codex-summary");
    const card = summary.closest("details");
    expect(card).not.toBeNull();
    expect(card?.open).toBe(false);
    // The browser reports the open through a `toggle` event one task after the
    // click; wait for it, as a person's next click always would.
    const toggled = new Promise((resolve) =>
      card?.addEventListener("toggle", resolve, { once: true }),
    );
    await fireEvent.click(summary);
    await toggled;
    expect(card?.open).toBe(true);

    await fireEvent.change(screen.getByTestId("settings-selection-codex-model-current"), {
      target: { value: "gpt-5.6-terra" },
    });
    await waitFor(() =>
      expect(screen.getByTestId("agent-defaults-codex-summary")).toHaveTextContent("GPT-5.6 Terra"),
    );
    expect(screen.getByTestId("agent-defaults-codex-summary").closest("details")).toBe(card);
    expect(card?.open).toBe(true);

    await fireEvent.change(screen.getByTestId("settings-selection-codex-effort-current"), {
      target: { value: "high" },
    });
    await waitFor(() =>
      expect(screen.getByTestId("agent-defaults-codex-summary")).toHaveTextContent("High"),
    );
    expect(card?.open).toBe(true);
  });

  it("shows the Antigravity quick choices and defaults", async () => {
    render(SettingsView, { props: { onClose: vi.fn(), section: "agents" } });
    await fireEvent.click(screen.getByTestId("agent-defaults-antigravity-summary"));

    expect(
      screen.getByTestId("settings-selection-antigravity-model-choice-gemini-4-argon"),
    ).toHaveAttribute("aria-pressed", "true");
    expect(
      screen.getByTestId("settings-selection-antigravity-model-choice-gemini-3.8-flash"),
    ).toHaveAttribute("aria-pressed", "true");
    expect(screen.getByTestId("settings-selection-antigravity-effort-current")).toHaveValue(
      "medium",
    );
  });

  it("refuses an invalid Antigravity default before it can leak through another save", async () => {
    const stored = {
      ...DEFAULT_AGENT_SELECTIONS.antigravity,
      model_choices: [...DEFAULT_AGENT_SELECTIONS.antigravity.model_choices, "medium-only-model"],
      default_model: "medium-only-model",
      default_effort: "medium",
    };
    preferences.agent_defaults.antigravity = structuredClone(stored);
    const view = render(SettingsView, { props: { onClose: vi.fn(), section: "agents" } });
    await fireEvent.click(screen.getByTestId("agent-defaults-antigravity-summary"));
    const effortDefault = screen.getByTestId("settings-selection-antigravity-effort-current");
    expect(within(effortDefault).getByRole("option", { name: "High" })).toBeDisabled();
    await fireEvent.change(effortDefault, { target: { value: "high" } });
    expect(invokeMock.mock.calls.filter(([command]) => command === "set_preferences")).toHaveLength(
      0,
    );

    await view.rerender({ section: "prompts" });
    await fireEvent.click(screen.getByTestId("show-builtins-toggle"));
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("set_preferences", {
        preferences: expect.objectContaining({
          agent_defaults: expect.objectContaining({ antigravity: stored }),
        }),
      }),
    );
  });

  it("keeps an invalid Antigravity repair local until the default becomes valid", async () => {
    const storedDefaults = {
      model_choices: ["medium-only-model"],
      effort_choices: ["high"],
      default_model: "medium-only-model",
      default_effort: "high",
    };
    preferences.agent_defaults.antigravity = structuredClone(storedDefaults);
    const view = render(SettingsView, { props: { onClose: vi.fn(), section: "agents" } });
    const antigravitySummary = screen.getByTestId("agent-defaults-antigravity-summary");
    await fireEvent.click(antigravitySummary);

    expect(screen.getByTestId("settings-selection-antigravity-invalid")).toBeInTheDocument();
    await fireEvent.click(
      screen.getByTestId("settings-selection-antigravity-effort-choice-medium"),
    );
    await waitFor(() =>
      expect(
        screen.getByTestId("settings-selection-antigravity-effort-choice-medium"),
      ).toHaveAttribute("aria-pressed", "true"),
    );
    expect(invokeMock.mock.calls.filter(([command]) => command === "set_preferences")).toHaveLength(
      0,
    );

    await fireEvent.click(antigravitySummary);
    await fireEvent.click(antigravitySummary);
    expect(
      screen.getByTestId("settings-selection-antigravity-effort-choice-medium"),
    ).toHaveAttribute("aria-pressed", "true");

    await view.rerender({ section: "prompts" });
    await fireEvent.click(screen.getByTestId("show-builtins-toggle"));
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("set_preferences", {
        preferences: expect.objectContaining({
          agent_defaults: expect.objectContaining({ antigravity: storedDefaults }),
        }),
      }),
    );

    await view.rerender({ section: "agents" });
    await fireEvent.change(screen.getByTestId("settings-selection-antigravity-effort-current"), {
      target: { value: "medium" },
    });
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("set_preferences", {
        preferences: expect.objectContaining({
          agent_defaults: expect.objectContaining({
            antigravity: {
              ...storedDefaults,
              effort_choices: ["high", "medium"],
              default_effort: "medium",
            },
          }),
        }),
      }),
    );
  });

  it("discards an incomplete default repair when Settings is closed", async () => {
    preferences.agent_defaults.antigravity = {
      model_choices: ["medium-only-model"],
      effort_choices: ["high"],
      default_model: "medium-only-model",
      default_effort: "high",
    };
    const view = render(SettingsView, { props: { onClose: vi.fn(), section: "agents" } });
    await fireEvent.click(screen.getByTestId("agent-defaults-antigravity-summary"));
    await fireEvent.click(
      screen.getByTestId("settings-selection-antigravity-effort-choice-medium"),
    );
    await waitFor(() =>
      expect(
        screen.getByTestId("settings-selection-antigravity-effort-choice-medium"),
      ).toHaveAttribute("aria-pressed", "true"),
    );

    view.unmount();
    render(SettingsView, { props: { onClose: vi.fn(), section: "agents" } });
    await fireEvent.click(screen.getByTestId("agent-defaults-antigravity-summary"));
    expect(
      screen.getByTestId("settings-selection-antigravity-effort-choice-medium"),
    ).toHaveAttribute("aria-pressed", "false");
  });

  it("keeps the effort default independent when the default model has no effort axis", async () => {
    preferences.agent_defaults.antigravity = {
      model_choices: ["no-effort-model", "three-level-model"],
      effort_choices: ["high", "medium"],
      default_model: "no-effort-model",
      default_effort: "high",
    };
    render(SettingsView, { props: { onClose: vi.fn(), section: "agents" } });
    await fireEvent.click(screen.getByTestId("agent-defaults-antigravity-summary"));

    const effortDefault = screen.getByTestId("settings-selection-antigravity-effort-current");
    expect(within(effortDefault).getByRole("option", { name: "Medium" })).toBeEnabled();
    await fireEvent.change(effortDefault, { target: { value: "medium" } });

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("set_preferences", {
        preferences: expect.objectContaining({
          agent_defaults: expect.objectContaining({
            antigravity: {
              model_choices: ["no-effort-model", "three-level-model"],
              effort_choices: ["high", "medium"],
              default_model: "no-effort-model",
              default_effort: "medium",
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

    const view = render(SettingsView, { props: { onClose: vi.fn(), section: "agents" } });
    expect(screen.getByTestId("agent-defaults-loading")).toBeInTheDocument();
    expect(screen.queryByTestId("settings-selection-claude_code-model")).toBeNull();
    await view.rerender({ section: "general" });
    expect(screen.getByTestId("external-editor-command")).toBeDisabled();
    expect(screen.getByTestId("external-terminal-app")).toHaveAttribute("aria-disabled", "true");
    expect(screen.getByTestId("external-terminal-app-option-terminal")).toBeDisabled();
    expect(screen.getByTestId("external-terminal-app-option-iterm")).toBeDisabled();
    await view.rerender({ section: "notifications" });
    expect(screen.getByTestId("notify-toggle")).toBeDisabled();
    expect(screen.getByTestId("notify-while-focused-toggle")).toBeDisabled();
    expect(screen.getByTestId("auto-reading-mode-toggle")).toBeDisabled();
    await view.rerender({ section: "prompts" });
    expect(screen.getByTestId("show-builtins-toggle")).toBeDisabled();
    await view.rerender({ section: "agents" });
    expect(screen.getByTestId("claude-chrome-toggle")).toBeDisabled();

    resolvePreferences({
      editor_command: "zed",
      terminal_app: "iTerm",
      diff_style: "unified",
      show_builtin_prompts: true,
      show_builtin_workflows: true,
      claude_chrome_enabled: false,
      auto_reading_mode: false,
      notify_on_completion: true,
      notify_while_focused: false,
      agent_defaults: {
        ...structuredClone(DEFAULT_AGENT_SELECTIONS),
        claude_code: {
          model_choices: ["sonnet"],
          effort_choices: ["low"],
          default_model: "sonnet",
          default_effort: "low",
        },
      },
    });

    await waitFor(() =>
      expect(
        screen.getByTestId("settings-selection-claude_code-model-choice-sonnet"),
      ).toHaveAttribute("aria-pressed", "true"),
    );
    await view.rerender({ section: "general" });
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

    await view.rerender({ section: "agents" });
    await fireEvent.click(
      screen.getByTestId("settings-selection-claude_code-effort-choice-medium"),
    );
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("set_preferences", {
        preferences: expect.objectContaining({
          agent_defaults: expect.objectContaining({
            claude_code: {
              model_choices: ["sonnet"],
              effort_choices: ["low", "medium"],
              default_model: "sonnet",
              default_effort: "low",
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
          show_builtin_prompts: true,
          show_builtin_workflows: true,
          claude_chrome_enabled: false,
          auto_reading_mode: false,
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
          show_builtin_prompts: true,
          show_builtin_workflows: true,
          claude_chrome_enabled: false,
          auto_reading_mode: false,
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
          show_builtin_prompts: true,
          show_builtin_workflows: true,
          claude_chrome_enabled: false,
          auto_reading_mode: false,
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
          show_builtin_prompts: true,
          show_builtin_workflows: true,
          claude_chrome_enabled: false,
          auto_reading_mode: false,
          notify_on_completion: true,
          notify_while_focused: false,
          agent_defaults: expect.any(Object),
        },
      }),
    );
  });

  it("saves independent built-in visibility choices on their own pages", async () => {
    const view = render(SettingsView, { props: { onClose: vi.fn(), section: "prompts" } });
    const promptToggle = screen.getByRole("switch", { name: "Show built-in prompts" });
    expect(
      screen.queryByRole("switch", { name: "Show built-in workflows" }),
    ).not.toBeInTheDocument();
    expect(promptToggle).toHaveAttribute("aria-checked", "true");

    await fireEvent.click(promptToggle);
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("set_preferences", {
        preferences: expect.objectContaining({
          show_builtin_prompts: false,
          show_builtin_workflows: true,
        }),
      }),
    );
    await view.rerender({ section: "workflows" });
    const workflowToggle = screen.getByRole("switch", { name: "Show built-in workflows" });
    expect(screen.queryByRole("switch", { name: "Show built-in prompts" })).not.toBeInTheDocument();
    expect(workflowToggle).toHaveAttribute("aria-checked", "true");
    await fireEvent.click(workflowToggle);
    await waitFor(() =>
      expect(invokeMock).toHaveBeenLastCalledWith("set_preferences", {
        preferences: expect.objectContaining({
          show_builtin_prompts: false,
          show_builtin_workflows: false,
        }),
      }),
    );
    await view.rerender({ section: "prompts" });
    expect(promptToggle).toBeVisible();
    expect(promptToggle).toHaveAttribute("aria-checked", "false");
    await fireEvent.click(promptToggle);
    await waitFor(() =>
      expect(invokeMock).toHaveBeenLastCalledWith("set_preferences", {
        preferences: expect.objectContaining({
          show_builtin_prompts: true,
          show_builtin_workflows: false,
        }),
      }),
    );
    await view.rerender({ section: "workflows" });
    expect(workflowToggle).toHaveAttribute("aria-checked", "false");
  });

  it("toggles claude_chrome_enabled and persists the change", async () => {
    render(SettingsView, { props: { onClose: vi.fn(), section: "agents" } });
    const toggle = screen.getByTestId("claude-chrome-toggle");
    // Browser access is opt-in: it must not be on until the user says so.
    expect(toggle).toHaveAttribute("aria-checked", "false");

    await fireEvent.click(toggle);
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("set_preferences", {
        preferences: expect.objectContaining({ claude_chrome_enabled: true }),
      }),
    );
    expect(toggle).toHaveAttribute("aria-checked", "true");
  });

  it("reflects a persisted claude_chrome_enabled from the backend", async () => {
    // Not ready → SettingsView triggers the load, so this covers the hydration
    // path rather than just local toggle state.
    prefsTesting.reset({ ready: false });
    invokeMock.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
      if (cmd === "get_preferences")
        return {
          editor_command: "code",
          terminal_app: "Terminal",
          diff_style: "unified",
          show_builtin_prompts: true,
          show_builtin_workflows: true,
          claude_chrome_enabled: true,
          auto_reading_mode: false,
          notify_on_completion: true,
          notify_while_focused: false,
          agent_defaults: DEFAULT_AGENT_SELECTIONS,
        };
      return defaultInvoke(cmd, args);
    });

    render(SettingsView, { props: { onClose: vi.fn(), section: "agents" } });

    await waitFor(() =>
      expect(screen.getByTestId("claude-chrome-toggle")).toHaveAttribute("aria-checked", "true"),
    );
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
    render(SettingsView, { props: { onClose: vi.fn(), section: "shortcuts" } });
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
    const notifications = screen.getByText("Toggle notifications").parentElement!;
    expect(Array.from(notifications.querySelectorAll("kbd"), (key) => key.textContent)).toEqual([
      "⌘",
      "⇧",
      "M",
    ]);
    expect(screen.getByText("Cycle to previous / next pane")).toBeInTheDocument();
  });

  it("shows the local prompts folder and opens it in Finder", async () => {
    render(SettingsView, { props: { onClose: vi.fn(), section: "prompts" } });

    await waitFor(() =>
      expect(screen.getByTestId("local-prompts-dir")).toHaveTextContent(
        "/Users/test/Library/Application Support/switchboard/prompts",
      ),
    );

    await fireEvent.click(screen.getByTestId("local-prompts-open"));

    expect(invokeMock).toHaveBeenCalledWith("open_local_prompts_dir", undefined);
  });

  it("shows the user-global workflows folder and opens it in Finder", async () => {
    render(SettingsView, { props: { onClose: vi.fn(), section: "workflows" } });

    await waitFor(() =>
      expect(screen.getByTestId("workflows-dir")).toHaveTextContent(
        "/Users/test/Library/Application Support/switchboard/workflows",
      ),
    );

    await fireEvent.click(screen.getByTestId("workflows-open"));

    expect(invokeMock).toHaveBeenCalledWith("open_workflows_dir", undefined);
  });

  it("previews and copies the engineering orchestrator prompt from the Agent MCP Server panel", async (): Promise<void> => {
    render(SettingsView, { props: { onClose: vi.fn(), section: "agent-access" } });

    const help = screen.getByRole("button", {
      name: "Read and copy the engineering orchestrator prompt",
    });
    const preview = screen.getByTestId("orchestrator-prompt");
    const copy = screen.getByTestId("orchestrator-prompt-copy");
    expect(preview).not.toBeVisible();
    await fireEvent.click(help);
    expect(preview).toBeVisible();
    expect(preview).toHaveTextContent("You are my stand-in");
    expect(preview).toHaveTextContent("principal engineer");
    await fireEvent.click(copy);
    await waitFor(() => expect(copyTextMock).toHaveBeenCalledOnce());
    expect(copyTextMock.mock.calls[0]![0]).toBe(ORCHESTRATOR_PROMPT);
  });

  it("opens the data science orchestrator prompt on its own and copies it", async (): Promise<void> => {
    render(SettingsView, { props: { onClose: vi.fn(), section: "agent-access" } });

    const help = screen.getByRole("button", {
      name: "Read and copy the data science orchestrator prompt",
    });
    const preview = screen.getByTestId("data-science-orchestrator-prompt");
    const engineering = screen.getByTestId("orchestrator-prompt");
    expect(preview).not.toBeVisible();
    await fireEvent.click(help);
    expect(preview).toBeVisible();
    expect(engineering).not.toBeVisible();
    expect(preview).toHaveTextContent("principal data scientist");
    expect(preview).toHaveTextContent("A result is a claim");
    await fireEvent.click(screen.getByTestId("data-science-orchestrator-prompt-copy"));
    await waitFor(() => expect(copyTextMock).toHaveBeenCalledOnce());
    expect(copyTextMock.mock.calls[0]![0]).toBe(DATA_SCIENCE_ORCHESTRATOR_PROMPT);
    expect(DATA_SCIENCE_ORCHESTRATOR_PROMPT).not.toBe(ORCHESTRATOR_PROMPT);
    expect(DATA_SCIENCE_ORCHESTRATOR_PROMPT).toContain("## The review loop");
  });

  it("previews and copies local prompt instructions with the folder once it loads", async (): Promise<void> => {
    let resolveFolder!: (path: string) => void;
    const folder = new Promise<string>((resolve) => {
      resolveFolder = resolve;
    });
    invokeMock.mockImplementation(async (cmd: string, args?: Record<string, unknown>) =>
      cmd === "local_prompts_dir" ? folder : defaultInvoke(cmd, args),
    );
    render(SettingsView, { props: { onClose: vi.fn(), section: "prompts" } });

    const help = screen.getByRole("button", { name: "How to create local prompts" });
    const preview = screen.getByTestId("local-prompt-authoring-prompt");
    const copy = screen.getByTestId("local-prompt-authoring-copy");
    expect(preview).not.toBeVisible();
    await fireEvent.click(help);
    expect(preview).toBeVisible();
    expect(copy).toBeVisible();
    await fireEvent.click(copy);
    await waitFor(() => expect(copyTextMock).toHaveBeenCalledOnce());
    expect(copyTextMock.mock.calls[0]![0]).toContain("Before saving, ask me for the folder path");

    const destination = "/Users/test/My prompt library";
    resolveFolder(destination);
    await waitFor(() => expect(preview).toHaveTextContent(destination));
    await fireEvent.click(copy);
    await waitFor(() => expect(copyTextMock).toHaveBeenCalledTimes(2));
    const copied = copyTextMock.mock.calls[1]![0];
    expect(copied).toBe(preview.textContent);
    expect(copied).toContain(destination);
    expect(copied).not.toContain("Before saving, ask me for the folder path");
    expect(copied).toContain("---\nname: explain-code");
    expect(copied).toContain("{{ target }}");
    expect(copied).toContain("Sync prompts");
    await fireEvent.click(help);
    expect(preview).not.toBeVisible();
  });

  it("previews and copies the workflow authoring prompt", async () => {
    render(SettingsView, { props: { onClose: vi.fn(), section: "workflows" } });

    const help = screen.getByRole("button", { name: "How to create and run workflows" });
    expect(help).toHaveAttribute("aria-expanded", "false");
    expect(screen.getByTestId("workflow-authoring-copy")).not.toBeVisible();
    expect(screen.getByTestId("workflow-authoring-prompt")).not.toBeVisible();
    expect(screen.getByTestId("workflows-open")).toBeVisible();
    await fireEvent.click(help);
    expect(screen.getByTestId("workflow-authoring-copy")).toBeVisible();

    expect(screen.getByTestId("workflow-authoring-prompt")).toBeVisible();

    await waitFor(() => expect(screen.getByTestId("workflows-dir")).toBeInTheDocument());
    await fireEvent.click(screen.getByTestId("workflow-authoring-copy"));
    await waitFor(() => expect(copyTextMock).toHaveBeenCalledOnce());

    const copied = copyTextMock.mock.calls[0]![0];
    expect(copied).toContain(WORKFLOW_AUTHORING_GUIDE_URL);
    expect(copied).toContain("/Users/test/Library/Application Support/switchboard/workflows");

    expect(screen.getByTestId("workflow-authoring-prompt").textContent).toBe(copied);
    await fireEvent.click(help);
    expect(screen.getByTestId("workflow-authoring-prompt")).not.toBeVisible();
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
    render(SettingsView, { props: { onClose: vi.fn(), section: "notifications" } });
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
    render(SettingsView, { props: { onClose: vi.fn(), section: "notifications" } });
    const master = await screen.findByTestId("notify-toggle");
    const nested = screen.getByTestId("notify-while-focused-toggle");
    expect(nested).not.toBeDisabled();

    await fireEvent.click(master);
    await waitFor(() => expect(nested).toBeDisabled());
  });

  it("the auto-reading-mode toggle is off by default and persists a flip", async () => {
    render(SettingsView, { props: { onClose: vi.fn(), section: "notifications" } });
    const toggle = await screen.findByTestId("auto-reading-mode-toggle");
    await waitFor(() => expect(toggle).toHaveAttribute("aria-checked", "false"));

    await fireEvent.click(toggle);
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith(
        "set_preferences",
        expect.objectContaining({
          preferences: expect.objectContaining({ auto_reading_mode: true }),
        }),
      ),
    );
    expect(toggle).toHaveAttribute("aria-checked", "true");
  });

  it("keeps the auto-reading-mode toggle live when notifications are off entirely", async () => {
    // Unlike the background-projects toggle it is not a sub-clause of "notify
    // me": hiding the compose box after each send is meaningful on its own as a
    // focus mode, so turning notifications off must not lock the user out of it.
    render(SettingsView, {
      props: { onClose: vi.fn(), section: "notifications" },
    });
    const master = await screen.findByTestId("notify-toggle");
    const autoReading = screen.getByTestId("auto-reading-mode-toggle");
    expect(autoReading).not.toBeDisabled();

    await fireEvent.click(master);
    await waitFor(() => expect(screen.getByTestId("notify-while-focused-toggle")).toBeDisabled());
    expect(autoReading).toBeVisible();
    expect(autoReading).not.toBeDisabled();
  });

  it("explains the two rules a user cannot infer from the toggle", async () => {
    // Both are load-bearing: without the first, testing the toggle while looking
    // at the app reads as broken; without the second, "sound but no banner" looks
    // impossible when it is actually a macOS setting.
    render(SettingsView, { props: { onClose: vi.fn(), section: "notifications" } });
    const section = screen.getByTestId("notification-prefs");
    const help = within(section).getByRole("button", { name: "How notifications work" });
    expect(within(section).getByText(/System Settings → Notifications/i)).not.toBeVisible();
    await fireEvent.click(help);
    expect(
      within(section).getByText(/The project on screen only notifies when reading mode is on/i),
    ).toBeVisible();
    expect(within(section).getByText(/System Settings → Notifications/i)).toBeVisible();
  });

  it("toggle reflects the stored preference and persists a flip", async () => {
    render(SettingsView, { props: { onClose: vi.fn(), section: "notifications" } });
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
    render(SettingsView, { props: { onClose: vi.fn(), section: "notifications" } });
    expect(await screen.findByTestId("notify-suppressed")).toBeInTheDocument();
  });

  it("stays quiet when notifications are available", async () => {
    // The sound-only configuration (alerts off, sound on) classifies as available
    // on the backend, so this is also the assertion that a working setup is not
    // reported as broken.
    render(SettingsView, { props: { onClose: vi.fn(), section: "notifications" } });
    await screen.findByTestId("notify-toggle");
    expect(screen.queryByTestId("notify-suppressed")).not.toBeInTheDocument();
  });

  it("stays quiet in an unbundled dev build rather than blaming the user's settings", async () => {
    // `unavailable` is not `suppressed`: nothing is misconfigured, the build just
    // isn't an installed app. Showing the blocking warning here would send a
    // developer to System Settings for no reason.
    withCommand("notification_availability", "unavailable");
    render(SettingsView, { props: { onClose: vi.fn(), section: "notifications" } });
    await screen.findByTestId("notify-toggle");
    expect(screen.queryByTestId("notify-suppressed")).not.toBeInTheDocument();
  });

  it("renders no hint when the availability probe itself fails", async () => {
    invokeMock.mockImplementation(async (c: string, args?: Record<string, unknown>) =>
      c === "notification_availability"
        ? Promise.reject(new Error("boom"))
        : defaultInvoke(c, args),
    );
    render(SettingsView, { props: { onClose: vi.fn(), section: "notifications" } });
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
    render(SettingsView, { props: { onClose: vi.fn(), section: "notifications" } });
    await fireEvent.click(await screen.findByTestId("notify-toggle"));

    expect(await screen.findByTestId("notify-save-error")).toBeInTheDocument();
  });

  it("reports a failed reading-mode save on Notifications", async (): Promise<void> => {
    failSaves();
    render(SettingsView, { props: { onClose: vi.fn(), section: "notifications" } });
    await fireEvent.click(screen.getByTestId("auto-reading-mode-toggle"));
    expect(await screen.findByTestId("reading-mode-save-error")).toBeVisible();
    expect(screen.queryByTestId("notify-save-error")).not.toBeInTheDocument();
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
    const view = render(SettingsView, {
      props: { onClose: vi.fn(), section: "notifications" },
    });
    await fireEvent.click(await screen.findByTestId("notify-toggle"));
    expect(await screen.findByTestId("notify-save-error")).toBeInTheDocument();

    invokeMock.mockImplementation(defaultInvoke);
    await view.rerender({ section: "general" });
    const editor = screen.getByTestId("external-editor-command") as HTMLInputElement;
    await fireEvent.input(editor, { target: { value: "cursor" } });
    await fireEvent.change(editor);

    await waitFor(() => expect(screen.queryByTestId("notify-save-error")).not.toBeInTheDocument());
  });
});
