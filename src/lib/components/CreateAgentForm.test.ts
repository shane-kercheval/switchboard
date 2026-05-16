import { describe, expect, it, vi } from "vitest";
import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen } from "@testing-library/svelte";
import CreateAgentForm from "./CreateAgentForm.svelte";
import type { AgentFormSubmit } from "./CreateAgentForm.types";

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
      name: "assistant",
      harness: "claude_code",
    } satisfies AgentFormSubmit);
  });

  it("create mode + Codex selection: submits {mode:create, harness:codex}", async () => {
    const { onSubmit } = renderForm();
    await fireEvent.click(screen.getByTestId("harness-codex"));
    await fireEvent.click(screen.getByTestId("confirm-create-agent"));
    expect(onSubmit).toHaveBeenCalledExactlyOnceWith({
      mode: "create",
      name: "assistant",
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
      name: "assistant",
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
      name: "assistant",
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
});
