import { describe, expect, it, vi, beforeEach } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/svelte";
import type { AgentRecord } from "$lib/types";

const stopAgentMock = vi.fn();
vi.mock("$lib/state/index.svelte", () => ({
  stopAgent: (id: string) => stopAgentMock(id),
}));

const agentSessionInfoMock = vi.fn();
const openSessionFileMock = vi.fn();
vi.mock("$lib/api", () => ({
  agentSessionInfo: (id: string) => agentSessionInfoMock(id),
  openSessionFile: async (id: string) => {
    openSessionFileMock(id);
  },
}));

const copyTextMock = vi.fn<(t: string) => Promise<void>>();
vi.mock("$lib/native", () => ({
  copyText: (t: string) => copyTextMock(t),
}));

const AGENT: AgentRecord = {
  id: "00000000-0000-7000-8000-000000000aaa",
  project_id: "00000000-0000-7000-8000-0000000000ff",
  name: "alice",
  harness: "claude_code",
  session_id: "00000000-0000-7000-8000-000000000001",
  created_at: "2026-05-16T00:00:00Z",
};

async function loadComponent() {
  return (await import("./AgentActionsMenu.svelte")).default;
}

beforeEach(() => {
  stopAgentMock.mockReset();
  agentSessionInfoMock.mockReset();
  openSessionFileMock.mockReset();
  copyTextMock.mockReset();
  copyTextMock.mockResolvedValue(undefined);
});

describe("AgentActionsMenu", () => {
  it("offers stop/open/resume when active and a session is resolvable", async () => {
    agentSessionInfoMock.mockResolvedValue({
      session_file: "/sessions/alice.jsonl",
      resume_command: "cd '/proj' && claude --resume abc --dangerously-skip-permissions",
    });
    const Component = await loadComponent();
    render(Component, { props: { agent: AGENT, active: true } });

    await fireEvent.click(screen.getByTestId("agent-actions-trigger"));
    await waitFor(() => expect(screen.getByTestId("agent-action-stop")).toBeInTheDocument());

    // Once the session info resolves, open + resume are enabled.
    await waitFor(() =>
      expect(screen.getByTestId("agent-action-open-session")).not.toHaveAttribute("data-disabled"),
    );
    expect(screen.getByTestId("agent-action-stop")).not.toHaveAttribute("data-disabled");

    await fireEvent.click(screen.getByTestId("agent-action-open-session"));
    expect(openSessionFileMock).toHaveBeenCalledWith(AGENT.id);
  });

  it("Stop agent invokes stopAgent", async () => {
    agentSessionInfoMock.mockResolvedValue({ session_file: null, resume_command: null });
    const Component = await loadComponent();
    render(Component, { props: { agent: AGENT, active: true } });

    await fireEvent.click(screen.getByTestId("agent-actions-trigger"));
    await waitFor(() => expect(screen.getByTestId("agent-action-stop")).toBeInTheDocument());
    await fireEvent.click(screen.getByTestId("agent-action-stop"));
    expect(stopAgentMock).toHaveBeenCalledWith(AGENT.id);
  });

  it("disables Stop when inactive and open/resume when no session is bound", async () => {
    agentSessionInfoMock.mockResolvedValue({ session_file: null, resume_command: null });
    const Component = await loadComponent();
    render(Component, { props: { agent: AGENT, active: false } });

    await fireEvent.click(screen.getByTestId("agent-actions-trigger"));
    await waitFor(() => expect(screen.getByTestId("agent-action-stop")).toBeInTheDocument());

    expect(screen.getByTestId("agent-action-stop")).toHaveAttribute("data-disabled");
    await waitFor(() =>
      expect(screen.getByTestId("agent-action-open-session")).toHaveAttribute("data-disabled"),
    );
    expect(screen.getByTestId("agent-action-resume")).toHaveAttribute("data-disabled");
  });

  it("resume panel shows the command, copies it, and warns more strongly when active", async () => {
    agentSessionInfoMock.mockResolvedValue({
      session_file: "/sessions/alice.jsonl",
      resume_command: "cd '/proj' && claude --resume abc --dangerously-skip-permissions",
    });
    const Component = await loadComponent();
    render(Component, { props: { agent: AGENT, active: true } });

    await fireEvent.click(screen.getByTestId("agent-actions-trigger"));
    await waitFor(() =>
      expect(screen.getByTestId("agent-action-resume")).not.toHaveAttribute("data-disabled"),
    );
    await fireEvent.click(screen.getByTestId("agent-action-resume"));

    await waitFor(() => expect(screen.getByTestId("resume-panel")).toBeInTheDocument());
    expect(screen.getByTestId("resume-command")).toHaveTextContent("claude --resume abc");
    // Active → the stronger collision warning.
    expect(screen.getByTestId("resume-warning-active")).toBeInTheDocument();

    await fireEvent.click(screen.getByTestId("resume-copy"));
    expect(copyTextMock).toHaveBeenCalledWith(
      "cd '/proj' && claude --resume abc --dangerously-skip-permissions",
    );
    // Confirmation appears only after the clipboard write resolves.
    await waitFor(() =>
      expect(screen.getByTestId("resume-copy")).toHaveAttribute("aria-label", "Copied"),
    );
  });

  it("does not confirm the copy when the clipboard write fails", async () => {
    copyTextMock.mockRejectedValueOnce(new Error("nope"));
    agentSessionInfoMock.mockResolvedValue({
      session_file: "/sessions/alice.jsonl",
      resume_command: "cd '/proj' && claude --resume abc --dangerously-skip-permissions",
    });
    const Component = await loadComponent();
    render(Component, { props: { agent: AGENT, active: true } });

    await fireEvent.click(screen.getByTestId("agent-actions-trigger"));
    await waitFor(() =>
      expect(screen.getByTestId("agent-action-resume")).not.toHaveAttribute("data-disabled"),
    );
    await fireEvent.click(screen.getByTestId("agent-action-resume"));
    await waitFor(() => expect(screen.getByTestId("resume-panel")).toBeInTheDocument());

    await fireEvent.click(screen.getByTestId("resume-copy"));
    await Promise.resolve();
    await Promise.resolve();

    expect(screen.getByTestId("resume-copy")).toHaveAttribute("aria-label", "Copy command");
  });
});
