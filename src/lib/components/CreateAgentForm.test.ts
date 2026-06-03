import { describe, expect, it, vi } from "vitest";
import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen } from "@testing-library/svelte";
import CreateAgentForm from "./CreateAgentForm.svelte";
import type { AgentFormSubmit } from "./CreateAgentForm.types";
import type { AgentRecord, HarnessAvailability } from "$lib/types";

function rosterAgent(name: string): AgentRecord {
  return {
    id: `id-${name}`,
    project_id: "p1",
    name,
    harness: "claude_code",
    session_locator: null,
    created_at: "2026-05-29T00:00:00Z",
  };
}

const CLAUDE_AVAILABLE: HarnessAvailability = { harness: "claude_code", binary: "available" };
const CLAUDE_BINARY_MISSING: HarnessAvailability = { harness: "claude_code", binary: "missing" };
const CODEX_AVAILABLE: HarnessAvailability = { harness: "codex", binary: "available" };
const CODEX_BINARY_MISSING: HarnessAvailability = { harness: "codex", binary: "missing" };
const CLAUDE_CHECKING: HarnessAvailability = { harness: "claude_code", binary: "checking" };
const CODEX_CHECKING: HarnessAvailability = { harness: "codex", binary: "checking" };
const GEMINI_AVAILABLE: HarnessAvailability = { harness: "gemini", binary: "available" };

const VALID_UUID = "019e2c5f-aaaa-7000-8000-000000000001";

function renderForm(): {
  onSubmit: ReturnType<typeof vi.fn>;
} {
  const onSubmit = vi.fn();
  render(CreateAgentForm, { props: { onSubmit } });
  return { onSubmit };
}

describe("CreateAgentForm", () => {
  it("create mode + Claude default: submits {mode:create, harness:claude_code}", async () => {
    const { onSubmit } = renderForm();
    await fireEvent.click(screen.getByTestId("confirm-create-agent"));
    expect(onSubmit).toHaveBeenCalledExactlyOnceWith({
      mode: "create",
      name: "claude-code",
      harness: "claude_code",
    } satisfies AgentFormSubmit);
  });

  it("create mode + Codex selection: submits {mode:create, harness:codex}", async () => {
    const { onSubmit } = renderForm();
    await fireEvent.click(screen.getByTestId("harness-codex"));
    await fireEvent.click(screen.getByTestId("confirm-create-agent"));
    expect(onSubmit).toHaveBeenCalledExactlyOnceWith({
      mode: "create",
      name: "codex",
      harness: "codex",
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

  it("create mode + Gemini selection: submits {mode:create, harness:gemini}", async () => {
    const { onSubmit } = renderForm();
    await fireEvent.click(screen.getByTestId("harness-gemini"));
    await fireEvent.click(screen.getByTestId("confirm-create-agent"));
    expect(onSubmit).toHaveBeenCalledExactlyOnceWith({
      mode: "create",
      name: "gemini",
      harness: "gemini",
    } satisfies AgentFormSubmit);
  });

  it("attach mode + Gemini selection: submits {mode:attach, harness:gemini, ...}", async () => {
    const { onSubmit } = renderForm();
    await fireEvent.click(screen.getByTestId("mode-attach"));
    await fireEvent.click(screen.getByTestId("harness-gemini"));
    const sessionInput = screen.getByTestId("attach-session-id") as HTMLInputElement;
    await fireEvent.input(sessionInput, { target: { value: VALID_UUID } });
    await fireEvent.click(screen.getByTestId("confirm-create-agent"));
    expect(onSubmit).toHaveBeenCalledExactlyOnceWith({
      mode: "attach",
      name: "gemini",
      harness: "gemini",
      existingSessionId: VALID_UUID,
    } satisfies AgentFormSubmit);
  });

  it("all three harnesses available: Gemini control enabled by default", () => {
    const onSubmit = vi.fn();
    render(CreateAgentForm, {
      props: {
        onSubmit,
        availability: {
          claude_code: CLAUDE_AVAILABLE,
          codex: CODEX_AVAILABLE,
          gemini: GEMINI_AVAILABLE,
        },
      },
    });
    const geminiControl = screen.getByTestId("harness-gemini") as HTMLInputElement;
    expect(geminiControl.disabled).toBe(false);
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
    expect(codexControl.closest("label")?.getAttribute("title")).toContain(
      "Codex not found on PATH",
    );

    // Claude control still selectable + submit succeeds with Claude.
    const claudeControl = screen.getByTestId("harness-claude_code") as HTMLInputElement;
    expect(claudeControl.disabled).toBe(false);
    await fireEvent.click(screen.getByTestId("confirm-create-agent"));
    expect(onSubmit).toHaveBeenCalledExactlyOnceWith({
      mode: "create",
      name: "claude-code",
      harness: "claude_code",
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
    // Realistic add-another-agent case: an auto-created "claude-code" already
    // exists, so the form opens with its default name already flagged.
    const onSubmit = vi.fn();
    render(CreateAgentForm, { props: { onSubmit, roster: [rosterAgent("claude-code")] } });
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

  it("aria-invalid tracks validity (incl. empty); aria-describedby links the message only when shown", async () => {
    const onSubmit = vi.fn();
    render(CreateAgentForm, { props: { onSubmit, roster: [rosterAgent("codex")] } });
    const nameInput = screen.getByTestId("agent-name") as HTMLInputElement;
    // Default "claude-code" is valid: not invalid, no description.
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
