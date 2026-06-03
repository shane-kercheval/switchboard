import { describe, expect, it, vi, beforeEach } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/svelte";
import type { AgentRecord } from "$lib/types";

const stopAgentMock = vi.fn();
vi.mock("$lib/state/index.svelte", () => ({
  stopAgent: (id: string) => stopAgentMock(id),
}));

const removeAgentMock = vi.fn<(id: string) => Promise<void>>();
vi.mock("$lib/state/workspace.svelte", () => ({
  removeAgent: (id: string) => removeAgentMock(id),
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
  session_locator: { uuid: "00000000-0000-7000-8000-000000000001" },
  created_at: "2026-05-16T00:00:00Z",
};

async function loadComponent() {
  return (await import("./AgentActionsMenu.svelte")).default;
}

beforeEach(() => {
  stopAgentMock.mockReset();
  removeAgentMock.mockReset();
  removeAgentMock.mockResolvedValue(undefined);
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

  it("disables Remove while the agent is active and explains why", async () => {
    agentSessionInfoMock.mockResolvedValue({ session_file: null, resume_command: null });
    const Component = await loadComponent();
    render(Component, { props: { agent: AGENT, active: true } });

    await fireEvent.click(screen.getByTestId("agent-actions-trigger"));
    const remove = await screen.findByTestId("agent-action-remove");
    expect(remove).toHaveAttribute("data-disabled");
    expect(remove).toHaveAttribute("title", "Stop the agent before removing it");
  });

  it("first Remove click swaps the menu to a focused confirm view, still open", async () => {
    agentSessionInfoMock.mockResolvedValue({ session_file: null, resume_command: null });
    const Component = await loadComponent();
    render(Component, { props: { agent: AGENT, active: false } });

    await fireEvent.click(screen.getByTestId("agent-actions-trigger"));
    await fireEvent.click(await screen.findByTestId("agent-action-remove"));

    // The menu stays open but its contents are replaced by the confirm view —
    // the live actions are gone, the prompt + Confirm/Cancel are shown, and the
    // backend hasn't been touched yet.
    expect(screen.getByTestId("agent-actions-menu")).toBeInTheDocument();
    expect(screen.getByTestId("agent-remove-prompt")).toBeInTheDocument();
    expect(screen.getByTestId("agent-remove-confirm")).toBeInTheDocument();
    expect(screen.getByTestId("agent-remove-cancel")).toBeInTheDocument();
    expect(screen.queryByTestId("agent-action-stop")).not.toBeInTheDocument();
    expect(screen.queryByTestId("agent-action-remove")).not.toBeInTheDocument();
    expect(removeAgentMock).not.toHaveBeenCalled();
  });

  it("confirming removal calls removeAgent and closes the menu", async () => {
    agentSessionInfoMock.mockResolvedValue({ session_file: null, resume_command: null });
    const Component = await loadComponent();
    render(Component, { props: { agent: AGENT, active: false } });

    await fireEvent.click(screen.getByTestId("agent-actions-trigger"));
    await fireEvent.click(await screen.findByTestId("agent-action-remove"));
    await fireEvent.click(screen.getByTestId("agent-remove-confirm"));

    expect(removeAgentMock).toHaveBeenCalledWith(AGENT.id);
    // Menu closes on success — its content unmounts.
    await waitFor(() => expect(screen.queryByTestId("agent-actions-menu")).not.toBeInTheDocument());
  });

  it("cancelling removal reverts the row without calling removeAgent", async () => {
    agentSessionInfoMock.mockResolvedValue({ session_file: null, resume_command: null });
    const Component = await loadComponent();
    render(Component, { props: { agent: AGENT, active: false } });

    await fireEvent.click(screen.getByTestId("agent-actions-trigger"));
    await fireEvent.click(await screen.findByTestId("agent-action-remove"));
    await fireEvent.click(screen.getByTestId("agent-remove-cancel"));

    expect(removeAgentMock).not.toHaveBeenCalled();
    // Back to the idle Remove item, menu still open.
    expect(await screen.findByTestId("agent-action-remove")).toBeInTheDocument();
    expect(screen.queryByTestId("agent-remove-confirm")).not.toBeInTheDocument();
  });

  it("surfaces a removal error and keeps the agent", async () => {
    agentSessionInfoMock.mockResolvedValue({ session_file: null, resume_command: null });
    removeAgentMock.mockRejectedValueOnce(new Error("registry locked"));
    const Component = await loadComponent();
    render(Component, { props: { agent: AGENT, active: false } });

    await fireEvent.click(screen.getByTestId("agent-actions-trigger"));
    await fireEvent.click(await screen.findByTestId("agent-action-remove"));
    await fireEvent.click(screen.getByTestId("agent-remove-confirm"));

    expect(removeAgentMock).toHaveBeenCalledWith(AGENT.id);
    const err = await screen.findByTestId("agent-remove-error");
    expect(err).toHaveTextContent("registry locked");
    // Menu stays open and reverts to the idle Remove item — the agent is kept.
    expect(screen.getByTestId("agent-action-remove")).toBeInTheDocument();
  });

  it("offers Rename only when onRename is given, and selecting it fires the callback", async () => {
    agentSessionInfoMock.mockResolvedValue({ session_file: null, resume_command: null });
    const Component = await loadComponent();
    const onRename = vi.fn();
    render(Component, { props: { agent: AGENT, active: false, onRename } });

    await fireEvent.click(screen.getByTestId("agent-actions-trigger"));
    const rename = await screen.findByTestId("agent-action-rename");
    await fireEvent.click(rename);
    expect(onRename).toHaveBeenCalledTimes(1);
  });

  it("omits Rename when no onRename callback is provided", async () => {
    agentSessionInfoMock.mockResolvedValue({ session_file: null, resume_command: null });
    const Component = await loadComponent();
    render(Component, { props: { agent: AGENT, active: false } });

    await fireEvent.click(screen.getByTestId("agent-actions-trigger"));
    await screen.findByTestId("agent-action-remove");
    expect(screen.queryByTestId("agent-action-rename")).not.toBeInTheDocument();
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
