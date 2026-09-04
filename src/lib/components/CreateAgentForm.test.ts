import { beforeEach, describe, expect, it, vi } from "vitest";
import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/svelte";
import CreateAgentForm from "./CreateAgentForm.svelte";
import type { AgentFormSubmit } from "./CreateAgentForm.types";
import type { AgentRecord, HarnessAvailability, Preferences } from "$lib/types";
import { preferences, _testing as preferencesTesting } from "$lib/preferences.svelte";
import { DEFAULT_AGENT_SELECTIONS } from "$lib/agentSelection";

const apiMocks = vi.hoisted(() => ({
  getPreferences: vi.fn(),
}));
vi.mock("$lib/api", async (importOriginal) => ({
  ...(await importOriginal<typeof import("$lib/api")>()),
  getPreferences: apiMocks.getPreferences,
}));

beforeEach(() => {
  preferencesTesting.reset({ ready: true });
  apiMocks.getPreferences.mockReset();
});

function rosterAgent(name: string): AgentRecord {
  return {
    id: `id-${name}`,
    project_id: "p1",
    name,
    harness: "claude_code",
    session_locator: null,
    model: null,
    effort: null,
    model_choices: [],
    effort_choices: [],
    created_at: "2026-05-29T00:00:00Z",
  };
}

const CLAUDE_AVAILABLE: HarnessAvailability = { harness: "claude_code", binary: "available" };
const CLAUDE_BINARY_MISSING: HarnessAvailability = { harness: "claude_code", binary: "missing" };
const CODEX_AVAILABLE: HarnessAvailability = { harness: "codex", binary: "available" };
const CODEX_BINARY_MISSING: HarnessAvailability = { harness: "codex", binary: "missing" };
const CLAUDE_CHECKING: HarnessAvailability = { harness: "claude_code", binary: "checking" };
const CODEX_CHECKING: HarnessAvailability = { harness: "codex", binary: "checking" };

const VALID_UUID = "019e2c5f-aaaa-7000-8000-000000000001";

function renderForm(): {
  onSubmit: ReturnType<typeof vi.fn>;
} {
  const onSubmit = vi.fn();
  render(CreateAgentForm, { props: { onSubmit } });
  return { onSubmit };
}

function pickerValue(testId: string): string {
  const el = screen.getByTestId(
    testId === "model-select"
      ? "create-selection-model-current"
      : testId === "effort-select"
        ? "create-selection-effort-current"
        : testId,
  );
  return el instanceof HTMLSelectElement ? el.value : (el.getAttribute("data-value") ?? "");
}

async function choosePicker(testId: string, value: string): Promise<void> {
  if (testId === "model-select" || testId === "effort-select") {
    const axis = testId === "model-select" ? "model" : "effort";
    const choice = screen.getByTestId(`create-selection-${axis}-choice-${value}`);
    if (choice.getAttribute("aria-pressed") !== "true") await fireEvent.click(choice);
    const current = screen.queryByTestId(`create-selection-${axis}-current`);
    if (current instanceof HTMLSelectElement) {
      await fireEvent.change(current, { target: { value } });
    }
    return;
  }
  const el = screen.getByTestId(testId);
  if (el instanceof HTMLSelectElement) {
    await fireEvent.change(el, { target: { value } });
  } else {
    await fireEvent.click(
      screen.getByTestId(`${testId}-option-${value === "" ? "no-override" : value}`),
    );
  }
}

describe("CreateAgentForm", () => {
  it("waits for saved defaults before initializing its editable draft", async () => {
    preferencesTesting.reset({ ready: false });
    let resolvePreferences!: (value: Preferences) => void;
    apiMocks.getPreferences.mockReturnValueOnce(
      new Promise<Preferences>((resolve) => {
        resolvePreferences = resolve;
      }),
    );

    renderForm();
    expect(screen.getByText("Loading defaults…")).toBeInTheDocument();
    expect(screen.getByTestId("confirm-create-agent")).toBeDisabled();

    resolvePreferences({
      editor_command: "code",
      terminal_app: "Terminal",
      diff_style: "unified",
      show_builtins: true,
      claude_chrome_enabled: false,
      auto_reading_mode: false,
      notify_on_completion: true,
      notify_while_focused: false,
      agent_defaults: {
        ...structuredClone(DEFAULT_AGENT_SELECTIONS),
        claude_code: {
          model_choices: ["sonnet", "haiku"],
          effort_choices: ["medium", "low"],
          default_model: "sonnet",
          default_effort: "medium",
        },
      },
    });

    await waitFor(() => expect(pickerValue("model-select")).toBe("sonnet"));
    expect(pickerValue("effort-select")).toBe("medium");
    expect(screen.getByTestId("agent-name")).toHaveValue("claude");
  });

  it("create mode + Claude default: submits {mode:create, harness:claude_code}", async () => {
    const { onSubmit } = renderForm();
    await fireEvent.click(screen.getByTestId("confirm-create-agent"));
    expect(onSubmit).toHaveBeenCalledExactlyOnceWith({
      mode: "create",
      name: "claude",
      harness: "claude_code",
      selection: {
        model: "opus",
        effort: "medium",
        model_choices: ["fable", "opus"],
        effort_choices: ["medium", "high"],
      },
    } satisfies AgentFormSubmit);
  });

  it("lists every harness in the picker", () => {
    renderForm();
    const picker = screen.getByTestId("harness-picker");
    const ids = Array.from(picker.querySelectorAll("input[type=radio]")).map((el) =>
      el.getAttribute("data-testid"),
    );
    expect(ids).toEqual(["harness-claude_code", "harness-codex", "harness-antigravity"]);
  });

  it("preselects saved quick choices and starting values", async () => {
    preferences.agent_defaults.claude_code = {
      model_choices: ["sonnet", "haiku"],
      effort_choices: ["medium", "low"],
      default_model: "sonnet",
      default_effort: "medium",
    };
    const { onSubmit } = renderForm();

    expect(pickerValue("model-select")).toBe("sonnet");
    expect(pickerValue("effort-select")).toBe("medium");
    expect(screen.getByTestId("create-selection-model-choice-haiku")).toHaveAttribute(
      "aria-pressed",
      "true",
    );
    expect(screen.getByTestId("create-selection-effort-choice-low")).toHaveAttribute(
      "aria-pressed",
      "true",
    );
    await fireEvent.click(screen.getByTestId("confirm-create-agent"));

    expect(onSubmit).toHaveBeenCalledExactlyOnceWith({
      mode: "create",
      name: "claude",
      harness: "claude_code",
      selection: {
        model: "sonnet",
        effort: "medium",
        model_choices: ["sonnet", "haiku"],
        effort_choices: ["medium", "low"],
      },
    } satisfies AgentFormSubmit);
  });

  it("submits create mode when Enter is pressed in the agent name field", async () => {
    const { onSubmit } = renderForm();
    const nameInput = screen.getByTestId("agent-name") as HTMLInputElement;
    await fireEvent.input(nameInput, { target: { value: "  my-agent  " } });
    await fireEvent.keyDown(nameInput, { key: "Enter" });

    expect(onSubmit).toHaveBeenCalledExactlyOnceWith({
      mode: "create",
      name: "my-agent",
      harness: "claude_code",
      selection: {
        model: "opus",
        effort: "medium",
        model_choices: ["fable", "opus"],
        effort_choices: ["medium", "high"],
      },
    } satisfies AgentFormSubmit);
  });

  it("does not submit from Enter when the agent name is invalid", async () => {
    const { onSubmit } = renderForm();
    const nameInput = screen.getByTestId("agent-name") as HTMLInputElement;
    await fireEvent.input(nameInput, { target: { value: "bad name" } });
    await fireEvent.keyDown(nameInput, { key: "Enter" });

    expect(onSubmit).not.toHaveBeenCalled();
  });

  it("create mode + Codex selection: submits {mode:create, harness:codex}", async () => {
    const { onSubmit } = renderForm();
    await fireEvent.click(screen.getByTestId("harness-codex"));
    await fireEvent.click(screen.getByTestId("confirm-create-agent"));
    expect(onSubmit).toHaveBeenCalledExactlyOnceWith({
      mode: "create",
      name: "codex",
      harness: "codex",
      selection: {
        model: "gpt-5.6-terra",
        effort: "medium",
        model_choices: ["gpt-5.6-sol", "gpt-5.6-terra"],
        effort_choices: ["medium", "high"],
      },
    } satisfies AgentFormSubmit);
  });

  it("attach mode: surfaces session-id field; submits {mode:attach,...} with the entered UUID", async () => {
    const { onSubmit } = renderForm();
    expect(screen.queryByTestId("attach-session-id")).not.toBeInTheDocument();
    await fireEvent.click(screen.getByTestId("mode-attach"));
    const sessionInput = screen.getByTestId("attach-session-id") as HTMLInputElement;
    await fireEvent.input(sessionInput, { target: { value: VALID_UUID } });
    await fireEvent.click(screen.getByTestId("confirm-create-agent"));
    expect(onSubmit).toHaveBeenCalledExactlyOnceWith({
      mode: "attach",
      name: "claude-code",
      harness: "claude_code",
      existingSessionId: VALID_UUID,
    } satisfies AgentFormSubmit);
  });

  it("submits attach mode when Enter is pressed in the session-id field", async () => {
    const { onSubmit } = renderForm();
    await fireEvent.click(screen.getByTestId("mode-attach"));
    const sessionInput = screen.getByTestId("attach-session-id") as HTMLInputElement;
    await fireEvent.input(sessionInput, { target: { value: VALID_UUID } });
    await fireEvent.keyDown(sessionInput, { key: "Enter" });

    expect(onSubmit).toHaveBeenCalledExactlyOnceWith({
      mode: "attach",
      name: "claude-code",
      harness: "claude_code",
      existingSessionId: VALID_UUID,
    } satisfies AgentFormSubmit);
  });

  it("attach mode: rejects malformed UUID — submit disabled and inline hint shown", async () => {
    renderForm();
    await fireEvent.click(screen.getByTestId("mode-attach"));
    const sessionInput = screen.getByTestId("attach-session-id") as HTMLInputElement;
    await fireEvent.input(sessionInput, { target: { value: "not-a-uuid" } });
    expect(screen.getByTestId("attach-session-id-error")).toBeInTheDocument();
    const submit = screen.getByTestId("confirm-create-agent") as HTMLButtonElement;
    expect(submit.disabled).toBe(true);
  });

  it("attach mode: empty session-id keeps submit disabled (no inline error until user types)", async () => {
    renderForm();
    await fireEvent.click(screen.getByTestId("mode-attach"));
    expect(screen.queryByTestId("attach-session-id-error")).not.toBeInTheDocument();
    const submit = screen.getByTestId("confirm-create-agent") as HTMLButtonElement;
    expect(submit.disabled).toBe(true);
  });

  it("renders backend error verbatim under data-testid='error'", () => {
    const onSubmit = vi.fn();
    render(CreateAgentForm, {
      props: {
        onSubmit,
        error: "SessionFileNotFound: ~/.claude/projects/...",
      },
    });
    expect(screen.getByTestId("error")).toHaveTextContent("SessionFileNotFound");
  });

  it("busy=true disables all inputs and re-labels the submit button", () => {
    const onSubmit = vi.fn();
    render(CreateAgentForm, { props: { onSubmit, busy: true } });
    const submit = screen.getByTestId("confirm-create-agent") as HTMLButtonElement;
    expect(submit.disabled).toBe(true);
    expect(submit).toHaveTextContent("Creating…");
  });

  it("busy=true + attach mode: submit re-labels to 'Attaching…'", async () => {
    const onSubmit = vi.fn();
    const { rerender } = render(CreateAgentForm, { props: { onSubmit } });
    await fireEvent.click(screen.getByTestId("mode-attach"));
    await rerender({ onSubmit, busy: true });
    expect(screen.getByTestId("confirm-create-agent")).toHaveTextContent("Attaching…");
  });

  it("attach mode + Codex selection: submits {mode:attach, harness:codex, ...}", async () => {
    const { onSubmit } = renderForm();
    await fireEvent.click(screen.getByTestId("mode-attach"));
    await fireEvent.click(screen.getByTestId("harness-codex"));
    const sessionInput = screen.getByTestId("attach-session-id") as HTMLInputElement;
    await fireEvent.input(sessionInput, { target: { value: VALID_UUID } });
    await fireEvent.click(screen.getByTestId("confirm-create-agent"));
    expect(onSubmit).toHaveBeenCalledExactlyOnceWith({
      mode: "attach",
      name: "codex",
      harness: "codex",
      existingSessionId: VALID_UUID,
    } satisfies AgentFormSubmit);
  });

  it("attach + Codex: a non-UUID thread id is accepted and submitted", async () => {
    const { onSubmit } = renderForm();
    await fireEvent.click(screen.getByTestId("harness-codex"));
    await fireEvent.click(screen.getByTestId("mode-attach"));
    const sessionInput = screen.getByTestId("attach-session-id") as HTMLInputElement;
    await fireEvent.input(sessionInput, { target: { value: "my-codex-thread-123" } });
    // No UUID error, submit enabled.
    expect(screen.queryByTestId("attach-session-id-error")).not.toBeInTheDocument();
    expect((screen.getByTestId("confirm-create-agent") as HTMLButtonElement).disabled).toBe(false);
    await fireEvent.click(screen.getByTestId("confirm-create-agent"));
    expect(onSubmit).toHaveBeenCalledExactlyOnceWith({
      mode: "attach",
      name: "codex",
      harness: "codex",
      existingSessionId: "my-codex-thread-123",
    } satisfies AgentFormSubmit);
  });

  it("attach + Codex: an empty session id keeps submit disabled", async () => {
    renderForm();
    await fireEvent.click(screen.getByTestId("harness-codex"));
    await fireEvent.click(screen.getByTestId("mode-attach"));
    expect((screen.getByTestId("confirm-create-agent") as HTMLButtonElement).disabled).toBe(true);
  });

  it("attach + Claude: a non-UUID id is still rejected (UUID error, submit disabled)", async () => {
    renderForm();
    await fireEvent.click(screen.getByTestId("mode-attach"));
    const sessionInput = screen.getByTestId("attach-session-id") as HTMLInputElement;
    await fireEvent.input(sessionInput, { target: { value: "my-codex-thread-123" } });
    expect(screen.getByTestId("attach-session-id-error")).toBeInTheDocument();
    expect((screen.getByTestId("confirm-create-agent") as HTMLButtonElement).disabled).toBe(true);
  });

  it("whitespace-only name keeps submit disabled (even with valid attach UUID)", async () => {
    renderForm();
    const nameInput = screen.getByTestId("agent-name") as HTMLInputElement;
    await fireEvent.input(nameInput, { target: { value: "   " } });
    const submit = screen.getByTestId("confirm-create-agent") as HTMLButtonElement;
    expect(submit.disabled).toBe(true);
  });

  it("Codex binary missing: Codex control disabled with tooltip; submit blocked", async () => {
    const onSubmit = vi.fn();
    render(CreateAgentForm, {
      props: {
        onSubmit,
        availability: { claude_code: CLAUDE_AVAILABLE, codex: CODEX_BINARY_MISSING },
      },
    });
    const codexControl = screen.getByTestId("harness-codex") as HTMLInputElement;
    expect(codexControl.disabled).toBe(true);
    const codexLabel = codexControl.closest("label");
    expect(codexLabel).not.toHaveAttribute("title");
    await fireEvent.pointerEnter(codexLabel!);
    expect(
      await screen.findByText(/Codex not found on PATH/, {}, { timeout: 1_500 }),
    ).toBeInTheDocument();

    // Claude control still selectable + submit succeeds with Claude.
    const claudeControl = screen.getByTestId("harness-claude_code") as HTMLInputElement;
    expect(claudeControl.disabled).toBe(false);
    await fireEvent.click(screen.getByTestId("confirm-create-agent"));
    expect(onSubmit).toHaveBeenCalledExactlyOnceWith({
      mode: "create",
      name: "claude",
      harness: "claude_code",
      selection: {
        model: "opus",
        effort: "medium",
        model_choices: ["fable", "opus"],
        effort_choices: ["medium", "high"],
      },
    } satisfies AgentFormSubmit);
  });

  it("Claude binary missing: Claude control disabled, Codex remains selectable", async () => {
    const onSubmit = vi.fn();
    render(CreateAgentForm, {
      props: {
        onSubmit,
        availability: { claude_code: CLAUDE_BINARY_MISSING, codex: CODEX_AVAILABLE },
      },
    });
    const claudeControl = screen.getByTestId("harness-claude_code") as HTMLInputElement;
    expect(claudeControl.disabled).toBe(true);
    const codexControl = screen.getByTestId("harness-codex") as HTMLInputElement;
    expect(codexControl.disabled).toBe(false);
  });

  it("selecting an unavailable harness shows inline gating message and disables submit", async () => {
    const onSubmit = vi.fn();
    render(CreateAgentForm, {
      props: {
        onSubmit,
        availability: { claude_code: CLAUDE_BINARY_MISSING, codex: CODEX_AVAILABLE },
      },
    });
    // Default selection is Claude (which is unavailable in this setup).
    expect(screen.getByTestId("harness-unavailable")).toHaveTextContent(
      "Claude Code not found on PATH",
    );
    const submit = screen.getByTestId("confirm-create-agent") as HTMLButtonElement;
    expect(submit.disabled).toBe(true);
  });

  it("checking state: both controls disabled, submit disabled, no inline message (silent disable)", () => {
    const onSubmit = vi.fn();
    render(CreateAgentForm, {
      props: {
        onSubmit,
        availability: { claude_code: CLAUDE_CHECKING, codex: CODEX_CHECKING },
      },
    });
    // Both controls are disabled — closes the pre-probe fail-open window.
    expect((screen.getByTestId("harness-claude_code") as HTMLInputElement).disabled).toBe(true);
    expect((screen.getByTestId("harness-codex") as HTMLInputElement).disabled).toBe(true);
    // Submit is gated alongside.
    const submit = screen.getByTestId("confirm-create-agent") as HTMLButtonElement;
    expect(submit.disabled).toBe(true);
    // No scary "Checking…" inline copy — checking returns null from the
    // reason helper so the silent-disable UX is intentional.
    expect(screen.queryByTestId("harness-unavailable")).not.toBeInTheDocument();
  });

  it("both harnesses available: no gating message, no control disabled", () => {
    const onSubmit = vi.fn();
    render(CreateAgentForm, {
      props: {
        onSubmit,
        availability: { claude_code: CLAUDE_AVAILABLE, codex: CODEX_AVAILABLE },
      },
    });
    expect(screen.queryByTestId("harness-unavailable")).not.toBeInTheDocument();
    expect((screen.getByTestId("harness-claude_code") as HTMLInputElement).disabled).toBe(false);
    expect((screen.getByTestId("harness-codex") as HTMLInputElement).disabled).toBe(false);
  });

  it("mode toggle attach → create → attach clears the stale session-id and error", async () => {
    renderForm();
    await fireEvent.click(screen.getByTestId("mode-attach"));
    const sessionInput = screen.getByTestId("attach-session-id") as HTMLInputElement;
    await fireEvent.input(sessionInput, { target: { value: "not-a-uuid" } });
    expect(screen.getByTestId("attach-session-id-error")).toBeInTheDocument();

    await fireEvent.click(screen.getByTestId("mode-create"));
    expect(screen.queryByTestId("attach-session-id")).not.toBeInTheDocument();

    await fireEvent.click(screen.getByTestId("mode-attach"));
    const sessionInputAgain = screen.getByTestId("attach-session-id") as HTMLInputElement;
    expect(sessionInputAgain.value).toBe("");
    expect(screen.queryByTestId("attach-session-id-error")).not.toBeInTheDocument();
  });

  it("duplicate name disables Create and shows the validation message", async () => {
    const onSubmit = vi.fn();
    render(CreateAgentForm, { props: { onSubmit, roster: [rosterAgent("codex")] } });
    const nameInput = screen.getByTestId("agent-name") as HTMLInputElement;
    await fireEvent.input(nameInput, { target: { value: "codex" } });
    expect(screen.getByTestId("agent-name-error")).toHaveTextContent("already exists");
    expect((screen.getByTestId("confirm-create-agent") as HTMLButtonElement).disabled).toBe(true);
  });

  it("duplicate detection is canonicalized (hyphen/case-insensitive)", async () => {
    const onSubmit = vi.fn();
    render(CreateAgentForm, { props: { onSubmit, roster: [rosterAgent("claude-code")] } });
    const nameInput = screen.getByTestId("agent-name") as HTMLInputElement;
    await fireEvent.input(nameInput, { target: { value: "Claude_Code" } });
    expect(screen.getByTestId("agent-name-error")).toBeInTheDocument();
    expect((screen.getByTestId("confirm-create-agent") as HTMLButtonElement).disabled).toBe(true);
  });

  it("fixing a duplicate name re-enables Create and clears the message", async () => {
    const onSubmit = vi.fn();
    render(CreateAgentForm, { props: { onSubmit, roster: [rosterAgent("codex")] } });
    const nameInput = screen.getByTestId("agent-name") as HTMLInputElement;
    await fireEvent.input(nameInput, { target: { value: "codex" } });
    expect((screen.getByTestId("confirm-create-agent") as HTMLButtonElement).disabled).toBe(true);
    await fireEvent.input(nameInput, { target: { value: "codex-2" } });
    expect(screen.queryByTestId("agent-name-error")).not.toBeInTheDocument();
    expect((screen.getByTestId("confirm-create-agent") as HTMLButtonElement).disabled).toBe(false);
  });

  it("invalid characters disable Create and show the message", async () => {
    const onSubmit = vi.fn();
    render(CreateAgentForm, { props: { onSubmit } });
    const nameInput = screen.getByTestId("agent-name") as HTMLInputElement;
    await fireEvent.input(nameInput, { target: { value: "bad name" } });
    expect(screen.getByTestId("agent-name-error")).toHaveTextContent("letters, numbers");
    expect((screen.getByTestId("confirm-create-agent") as HTMLButtonElement).disabled).toBe(true);
  });

  it("empty name disables Create without showing an error message (no mid-edit nag)", async () => {
    const onSubmit = vi.fn();
    render(CreateAgentForm, { props: { onSubmit } });
    const nameInput = screen.getByTestId("agent-name") as HTMLInputElement;
    await fireEvent.input(nameInput, { target: { value: "" } });
    expect(screen.queryByTestId("agent-name-error")).not.toBeInTheDocument();
    expect((screen.getByTestId("confirm-create-agent") as HTMLButtonElement).disabled).toBe(true);
  });

  it("flags the default name on open when it already collides with the roster", () => {
    // Realistic add-another-agent case: an auto-created "claude" already
    // exists, so the form opens with its default name already flagged.
    const onSubmit = vi.fn();
    render(CreateAgentForm, { props: { onSubmit, roster: [rosterAgent("claude")] } });
    expect(screen.getByTestId("agent-name-error")).toHaveTextContent("already exists");
    expect((screen.getByTestId("confirm-create-agent") as HTMLButtonElement).disabled).toBe(true);
  });

  it("submits the normalized (trimmed) name", async () => {
    const onSubmit = vi.fn();
    render(CreateAgentForm, { props: { onSubmit } });
    const nameInput = screen.getByTestId("agent-name") as HTMLInputElement;
    await fireEvent.input(nameInput, { target: { value: "  my-agent  " } });
    await fireEvent.click(screen.getByTestId("confirm-create-agent"));
    expect(onSubmit).toHaveBeenCalledExactlyOnceWith({
      mode: "create",
      name: "my-agent",
      harness: "claude_code",
      selection: {
        model: "opus",
        effort: "medium",
        model_choices: ["fable", "opus"],
        effort_choices: ["medium", "high"],
      },
    } satisfies AgentFormSubmit);
  });

  it("attach mode: a valid UUID with a duplicate name keeps submit disabled (both gates apply)", async () => {
    const onSubmit = vi.fn();
    render(CreateAgentForm, { props: { onSubmit, roster: [rosterAgent("codex")] } });
    await fireEvent.click(screen.getByTestId("mode-attach"));
    const sessionInput = screen.getByTestId("attach-session-id") as HTMLInputElement;
    await fireEvent.input(sessionInput, { target: { value: VALID_UUID } });
    const nameInput = screen.getByTestId("agent-name") as HTMLInputElement;
    await fireEvent.input(nameInput, { target: { value: "codex" } });
    // UUID is valid, so the only remaining gate is the duplicate name.
    expect(screen.queryByTestId("attach-session-id-error")).not.toBeInTheDocument();
    expect(screen.getByTestId("agent-name-error")).toBeInTheDocument();
    expect((screen.getByTestId("confirm-create-agent") as HTMLButtonElement).disabled).toBe(true);
  });

  // --- Model + effort pickers ------------------------------------------------

  it("create + Claude: model and effort pickers preselect the harness defaults", () => {
    renderForm();
    expect(pickerValue("model-select")).toBe("opus");
    expect(pickerValue("effort-select")).toBe("medium");
    // No unsupported-capability notes for a fully-capable harness.
    expect(screen.queryByTestId("model-note")).not.toBeInTheDocument();
    expect(screen.queryByTestId("effort-note")).not.toBeInTheDocument();
  });

  it("create + Codex: pickers preselect the configured default", async () => {
    renderForm();
    await fireEvent.click(screen.getByTestId("harness-codex"));
    expect(pickerValue("model-select")).toBe("gpt-5.6-terra");
    expect(pickerValue("effort-select")).toBe("medium");
  });

  it("create + Antigravity: model and effort are selectable, and submit carries them", async () => {
    // `agy` 1.1.x made `--model`/`--effort` usable headlessly without touching
    // the harness's own global config, so Antigravity now gets the same two
    // controls as Claude and Codex rather than a not-supported note.
    const { onSubmit } = renderForm();
    await fireEvent.click(screen.getByTestId("harness-antigravity"));
    expect(screen.getByTestId("create-selection-model")).toBeInTheDocument();
    expect(screen.getByTestId("create-selection-effort")).toBeInTheDocument();
    await fireEvent.click(screen.getByTestId("confirm-create-agent"));
    expect(onSubmit).toHaveBeenCalledExactlyOnceWith({
      mode: "create",
      name: "antigravity",
      harness: "antigravity",
      selection: {
        model: "gemini-3.8-flash",
        effort: "medium",
        model_choices: ["gemini-3.8-flash", "gemini-3.1-pro"],
        effort_choices: ["medium", "high"],
      },
    } satisfies AgentFormSubmit);
  });

  it("create + Antigravity: a model with no effort axis keeps future choices configurable", async () => {
    renderForm();
    await fireEvent.click(screen.getByTestId("harness-antigravity"));
    await choosePicker("model-select", "claude-sonnet-4-6");
    expect(screen.getByTestId("create-selection-effort-choice-high")).toBeEnabled();
    expect(screen.queryByTestId("create-selection-effort-current")).not.toBeInTheDocument();
    expect(screen.getByTestId("create-selection-effort-unavailable")).toHaveTextContent(
      "current model does not use reasoning effort",
    );
  });

  it("create + Antigravity: an effort-bearing model offers no Default and only its own levels", async () => {
    // Gemini 3.1 Pro has low/high but not medium, and `agy` rejects a bare
    // effort-bearing model — so "Default" must not be offered.
    renderForm();
    await fireEvent.click(screen.getByTestId("harness-antigravity"));
    // 3.1 Pro is not the default (3.7 Flash is), so select it explicitly —
    // it is the one curated model whose levels exclude `medium`.
    await choosePicker("model-select", "gemini-3.1-pro");
    expect(screen.getByTestId("create-selection-effort-choice-low")).toBeEnabled();
    expect(screen.getByTestId("create-selection-effort-choice-high")).toBeEnabled();
    expect(screen.getByTestId("create-selection-effort-choice-medium")).toBeEnabled();
    const effortCurrent = screen.getByTestId("create-selection-effort-current");
    expect(within(effortCurrent).getByRole("option", { name: "Medium" })).toBeDisabled();
  });

  it("create + Antigravity: converts independent defaults into a dispatch-ready pair", async () => {
    preferences.agent_defaults.antigravity = {
      model_choices: ["claude-sonnet-4-6", "gemini-3.8-flash"],
      effort_choices: ["high", "medium"],
      default_model: "claude-sonnet-4-6",
      default_effort: "high",
    };
    const { onSubmit } = renderForm();
    await fireEvent.click(screen.getByTestId("harness-antigravity"));

    expect(screen.getByTestId("create-selection-effort-unavailable")).toBeInTheDocument();
    await fireEvent.click(screen.getByTestId("confirm-create-agent"));
    expect(onSubmit).toHaveBeenCalledExactlyOnceWith({
      mode: "create",
      name: "antigravity",
      harness: "antigravity",
      selection: {
        model: "claude-sonnet-4-6",
        effort: null,
        model_choices: ["claude-sonnet-4-6", "gemini-3.8-flash"],
        effort_choices: ["high", "medium"],
      },
    } satisfies AgentFormSubmit);
  });

  it("create + Antigravity: blocks an unresolvable saved configuration", async () => {
    preferences.agent_defaults.antigravity = {
      model_choices: ["gemini-3.1-pro"],
      effort_choices: ["medium"],
      default_model: "gemini-3.1-pro",
      default_effort: "medium",
    };
    renderForm();
    await fireEvent.click(screen.getByTestId("harness-antigravity"));

    expect(screen.getByTestId("create-selection-invalid")).toBeInTheDocument();
    expect(screen.getByTestId("confirm-create-agent")).toBeDisabled();
  });

  it("create + Antigravity: switching to a narrower model clamps the effort", async () => {
    // `agy` fails the turn when the level isn't in the model's own set, so an
    // effort left over from the previous model has to be brought back into
    // range at selection time rather than discovered at dispatch. 3.8 Flash
    // takes low/medium/high; 3.1 Pro only low/high.
    const { onSubmit } = renderForm();
    await fireEvent.click(screen.getByTestId("harness-antigravity"));
    await choosePicker("model-select", "gemini-3.8-flash");
    await choosePicker("effort-select", "medium");
    expect(pickerValue("effort-select")).toBe("medium");

    await choosePicker("model-select", "gemini-3.1-pro");

    expect(pickerValue("effort-select")).toBe("high");
    // Pinned at the wire too: a clamp the picker shows but the payload drops
    // would still fail the turn.
    await fireEvent.click(screen.getByTestId("confirm-create-agent"));
    expect(onSubmit).toHaveBeenCalledExactlyOnceWith({
      mode: "create",
      name: "antigravity",
      harness: "antigravity",
      selection: {
        model: "gemini-3.1-pro",
        effort: "high",
        model_choices: ["gemini-3.8-flash", "gemini-3.1-pro"],
        effort_choices: ["medium", "high"],
      },
    } satisfies AgentFormSubmit);
  });

  it("changing the model and effort pickers submits the chosen values", async () => {
    const { onSubmit } = renderForm();
    await choosePicker("model-select", "sonnet");
    await choosePicker("effort-select", "max");
    await fireEvent.click(screen.getByTestId("confirm-create-agent"));
    expect(onSubmit).toHaveBeenCalledExactlyOnceWith({
      mode: "create",
      name: "claude",
      harness: "claude_code",
      selection: {
        model: "sonnet",
        effort: "max",
        model_choices: ["fable", "opus", "sonnet"],
        effort_choices: ["medium", "high", "max"],
      },
    } satisfies AgentFormSubmit);
  });

  it("switching harness resets a changed picker to the new harness default", async () => {
    const { onSubmit } = renderForm();
    // Change Claude's model away from the default, then switch to Codex.
    await choosePicker("model-select", "haiku");
    await fireEvent.click(screen.getByTestId("harness-codex"));
    // The stale Claude value is gone — Codex shows its own default.
    expect(pickerValue("model-select")).toBe("gpt-5.6-terra");
    await fireEvent.click(screen.getByTestId("confirm-create-agent"));
    expect(onSubmit).toHaveBeenCalledExactlyOnceWith({
      mode: "create",
      name: "codex",
      harness: "codex",
      selection: {
        model: "gpt-5.6-terra",
        effort: "medium",
        model_choices: ["gpt-5.6-sol", "gpt-5.6-terra"],
        effort_choices: ["medium", "high"],
      },
    } satisfies AgentFormSubmit);
  });

  it("attach mode: no model/effort controls; submits no model/effort (session left as-is)", async () => {
    const { onSubmit } = renderForm();
    await fireEvent.click(screen.getByTestId("mode-attach"));
    // Attach pins nothing: neither the pickers nor the unsupported-capability
    // notes are shown — model/effort are managed from the agent's actions menu.
    expect(screen.queryByTestId("create-selection")).not.toBeInTheDocument();
    expect(screen.queryByTestId("model-note")).not.toBeInTheDocument();
    expect(screen.queryByTestId("effort-note")).not.toBeInTheDocument();
    const sessionInput = screen.getByTestId("attach-session-id") as HTMLInputElement;
    await fireEvent.input(sessionInput, { target: { value: VALID_UUID } });
    await fireEvent.click(screen.getByTestId("confirm-create-agent"));
    expect(onSubmit).toHaveBeenCalledExactlyOnceWith({
      mode: "attach",
      name: "claude-code",
      harness: "claude_code",
      existingSessionId: VALID_UUID,
    } satisfies AgentFormSubmit);
  });

  it("attach hides the model/effort pickers; switching back to create shows them again", async () => {
    renderForm();
    await fireEvent.click(screen.getByTestId("mode-attach"));
    expect(screen.queryByTestId("create-selection")).not.toBeInTheDocument();
    await fireEvent.click(screen.getByTestId("mode-create"));
    // Untouched, so they're at the harness default — this is re-render, not reset
    // (see the draft-preservation test below).
    expect(pickerValue("model-select")).toBe("opus");
    expect(pickerValue("effort-select")).toBe("medium");
  });

  it("create model/effort selections survive a create → attach → create toggle (draft preservation)", async () => {
    const { onSubmit } = renderForm();
    await choosePicker("model-select", "haiku");
    await choosePicker("effort-select", "low");
    await fireEvent.click(screen.getByTestId("mode-attach"));
    await fireEvent.click(screen.getByTestId("mode-create"));
    // The user's picks are preserved, not reset to the harness default.
    expect(pickerValue("model-select")).toBe("haiku");
    expect(pickerValue("effort-select")).toBe("low");
    await fireEvent.click(screen.getByTestId("confirm-create-agent"));
    expect(onSubmit).toHaveBeenCalledExactlyOnceWith({
      mode: "create",
      name: "claude",
      harness: "claude_code",
      selection: {
        model: "haiku",
        effort: "low",
        model_choices: ["fable", "opus", "haiku"],
        effort_choices: ["medium", "high", "low"],
      },
    } satisfies AgentFormSubmit);
  });

  it("create: reducing both axes to one choice makes the name track the selection", async () => {
    renderForm();
    const nameInput = screen.getByTestId("agent-name") as HTMLInputElement;
    expect(nameInput.value).toBe("claude");
    await fireEvent.click(screen.getByTestId("create-selection-model-choice-fable"));
    await fireEvent.click(screen.getByTestId("create-selection-effort-choice-high"));
    expect(nameInput.value).toBe("opus-medium");
  });

  it("create: adding another choice switches the untouched auto-name to the harness", async () => {
    renderForm();
    const nameInput = screen.getByTestId("agent-name") as HTMLInputElement;
    expect(nameInput.value).toBe("claude");

    await fireEvent.click(screen.getByTestId("create-selection-model-choice-fable"));
    await fireEvent.click(screen.getByTestId("create-selection-effort-choice-high"));
    expect(nameInput.value).toBe("opus-medium");

    await fireEvent.click(screen.getByTestId("create-selection-model-choice-haiku"));
    expect(nameInput.value).toBe("claude");
  });

  it("create: switching harness re-derives the auto-name (incl. bare-name harnesses)", async () => {
    renderForm();
    const nameInput = screen.getByTestId("agent-name") as HTMLInputElement;
    await fireEvent.click(screen.getByTestId("harness-codex"));
    expect(nameInput.value).toBe("codex");
    await fireEvent.click(screen.getByTestId("harness-antigravity"));
    expect(nameInput.value).toBe("antigravity");
  });

  it("editing the name freezes it against later picker and harness changes", async () => {
    const { onSubmit } = renderForm();
    const nameInput = screen.getByTestId("agent-name") as HTMLInputElement;
    await fireEvent.input(nameInput, { target: { value: "my-thing" } });
    // Neither a picker change nor a harness switch overrides the user's name.
    await choosePicker("model-select", "sonnet");
    expect(nameInput.value).toBe("my-thing");
    await fireEvent.click(screen.getByTestId("create-selection-model-choice-haiku"));
    expect(nameInput.value).toBe("my-thing");
    await fireEvent.click(screen.getByTestId("harness-codex"));
    expect(nameInput.value).toBe("my-thing");
    await fireEvent.click(screen.getByTestId("confirm-create-agent"));
    expect(onSubmit).toHaveBeenCalledExactlyOnceWith({
      mode: "create",
      name: "my-thing",
      harness: "codex",
      selection: {
        model: "gpt-5.6-terra",
        effort: "medium",
        model_choices: ["gpt-5.6-sol", "gpt-5.6-terra"],
        effort_choices: ["medium", "high"],
      },
    } satisfies AgentFormSubmit);
  });

  it("aria-invalid tracks validity (incl. empty); aria-describedby links the message only when shown", async () => {
    const onSubmit = vi.fn();
    render(CreateAgentForm, { props: { onSubmit, roster: [rosterAgent("codex")] } });
    const nameInput = screen.getByTestId("agent-name") as HTMLInputElement;
    // Default "claude" is valid: not invalid, no description.
    expect(nameInput.getAttribute("aria-invalid")).toBe("false");
    expect(nameInput.getAttribute("aria-describedby")).toBeNull();

    // Empty: invalid for assistive tech, but no visible message/border (no nag).
    await fireEvent.input(nameInput, { target: { value: "" } });
    expect(nameInput.getAttribute("aria-invalid")).toBe("true");
    expect(screen.queryByTestId("agent-name-error")).not.toBeInTheDocument();
    expect(nameInput.getAttribute("aria-describedby")).toBeNull();

    // Duplicate: invalid and the message is linked.
    await fireEvent.input(nameInput, { target: { value: "codex" } });
    expect(nameInput.getAttribute("aria-invalid")).toBe("true");
    expect(nameInput.getAttribute("aria-describedby")).toBe("agent-name-error");
    expect(screen.getByTestId("agent-name-error")).toBeInTheDocument();
  });
});
