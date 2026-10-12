import { beforeEach, describe, expect, it, vi } from "vitest";
import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/svelte";
import AgentAccessSettings from "./AgentAccessSettings.svelte";
import type { AgentAccessStatus } from "$lib/types";

const invokeMock = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: Record<string, unknown>) => invokeMock(cmd, args),
}));

const copyTextMock = vi.fn((_text: string) => Promise.resolve());
vi.mock("$lib/native", () => ({
  copyText: (text: string) => copyTextMock(text),
}));

const TOKEN = "abcdefghijklmnopqrstuvwxyz0123456789ABCDEFG";

function off(overrides: Partial<AgentAccessStatus> = {}): AgentAccessStatus {
  return {
    enabled: false,
    port: 47_822,
    listening: false,
    error: null,
    token: null,
    url: "http://127.0.0.1:47822/mcp",
    ...overrides,
  };
}

function listening(overrides: Partial<AgentAccessStatus> = {}): AgentAccessStatus {
  return off({ enabled: true, listening: true, token: TOKEN, ...overrides });
}

/// A fake backend whose state the commands change, as the real one does.
let backend: AgentAccessStatus;

beforeEach(() => {
  backend = off();
  copyTextMock.mockClear();
  invokeMock.mockReset();
  invokeMock.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
    switch (cmd) {
      case "mcp_status":
        return backend;
      case "mcp_set_enabled":
        backend = args?.enabled
          ? listening({ port: backend.port })
          : { ...backend, enabled: false, listening: false };
        return backend;
      case "mcp_set_port": {
        const port = args?.port as number;
        backend = { ...backend, port, url: `http://127.0.0.1:${port}/mcp` };
        return backend;
      }
      case "mcp_regenerate_token":
        backend = { ...backend, token: "fresh-token-0000" };
        return backend;
      default:
        throw new Error(`unexpected command ${cmd}`);
    }
  });
});

describe("AgentAccessSettings", () => {
  it("starts off with no token and no client configuration", async () => {
    render(AgentAccessSettings);
    await waitFor(() => expect(screen.getByTestId("agent-access-state")).toHaveTextContent("Off"));
    expect(screen.getByTestId("agent-access-toggle")).toHaveAttribute("aria-checked", "false");
    expect(screen.queryByTestId("agent-access-token")).toBeNull();
    expect(screen.queryByTestId("agent-access-clients")).toBeNull();
  });

  it("turning it on shows where it listens, the masked token, and each client's configuration", async () => {
    render(AgentAccessSettings);
    await waitFor(() => screen.getByTestId("agent-access-toggle"));
    await fireEvent.click(screen.getByTestId("agent-access-toggle"));

    await waitFor(() =>
      expect(screen.getByTestId("agent-access-state")).toHaveTextContent(
        "Listening on http://127.0.0.1:47822/mcp",
      ),
    );
    expect(invokeMock).toHaveBeenCalledWith("mcp_set_enabled", { enabled: true });
    const token = screen.getByTestId("agent-access-token");
    expect(token).not.toHaveTextContent(TOKEN);
    expect(token).toHaveTextContent("DEFG");
    await fireEvent.click(screen.getByTestId("agent-access-token-reveal"));
    expect(screen.getByTestId("agent-access-token")).toHaveTextContent(TOKEN);

    expect(screen.getByTestId("agent-access-claude-code")).toHaveTextContent(
      `claude mcp add --transport http switchboard http://127.0.0.1:47822/mcp --header "Authorization: Bearer ${TOKEN}"`,
    );
    expect(screen.getByTestId("agent-access-codex")).toHaveTextContent(
      'url = "http://127.0.0.1:47822/mcp"',
    );
    expect(screen.getByTestId("agent-access-codex-env")).toHaveTextContent(
      `export SWITCHBOARD_TOKEN=${TOKEN}`,
    );
    expect(screen.getByTestId("agent-access-claude-desktop")).toHaveTextContent(
      `"AUTH_HEADER": "Bearer ${TOKEN}"`,
    );
  });

  it("copies the token itself, not the masked text", async () => {
    backend = listening();
    render(AgentAccessSettings);
    await waitFor(() => screen.getByTestId("agent-access-token-copy"));
    await fireEvent.click(screen.getByTestId("agent-access-token-copy"));
    await waitFor(() => expect(copyTextMock).toHaveBeenCalledWith(TOKEN));
  });

  it("shows a port in use as an error while the setting stays on", async () => {
    backend = off({
      enabled: true,
      token: TOKEN,
      error: "Port 47822 is already in use by another program. Choose a different port.",
    });
    render(AgentAccessSettings);
    await waitFor(() =>
      expect(screen.getByTestId("agent-access-error")).toHaveTextContent("already in use"),
    );
    expect(screen.getByTestId("agent-access-toggle")).toHaveAttribute("aria-checked", "true");
    expect(screen.getByTestId("agent-access-state")).toHaveTextContent("not listening");
  });

  it("applies a new port on Enter and refuses one out of range without asking the backend", async () => {
    backend = listening();
    render(AgentAccessSettings);
    const input = await waitFor(() => screen.getByTestId("agent-access-port"));

    await fireEvent.input(input, { target: { value: "70000" } });
    await fireEvent.keyDown(input, { key: "Enter" });
    expect(screen.getByTestId("agent-access-action-error")).toHaveTextContent("1 to 65535");
    expect(invokeMock).not.toHaveBeenCalledWith("mcp_set_port", expect.anything());

    await fireEvent.input(input, { target: { value: "50000" } });
    await fireEvent.keyDown(input, { key: "Enter" });
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("mcp_set_port", { port: 50_000 }));
    await waitFor(() =>
      expect(screen.getByTestId("agent-access-state")).toHaveTextContent("127.0.0.1:50000"),
    );
    expect(screen.queryByTestId("agent-access-action-error")).toBeNull();
  });

  it("leaving the port unchanged sends nothing", async () => {
    backend = listening();
    render(AgentAccessSettings);
    const input = await waitFor(() => screen.getByTestId("agent-access-port"));
    await fireEvent.blur(input);
    expect(invokeMock).not.toHaveBeenCalledWith("mcp_set_port", expect.anything());
  });

  it("regenerating shows the new token in every client's configuration", async () => {
    backend = listening();
    render(AgentAccessSettings);
    await waitFor(() => screen.getByTestId("agent-access-regenerate"));
    await fireEvent.click(screen.getByTestId("agent-access-regenerate"));
    await fireEvent.click(screen.getByTestId("agent-access-token-reveal"));
    await waitFor(() =>
      expect(screen.getByTestId("agent-access-claude-code")).toHaveTextContent(
        "Bearer fresh-token-0000",
      ),
    );
    expect(screen.getByTestId("agent-access-codex-env")).toHaveTextContent(
      "SWITCHBOARD_TOKEN=fresh-token-0000",
    );
  });

  it("keeps the token off the screen until Show, while every Copy button copies it", async () => {
    backend = listening();
    const { container } = render(AgentAccessSettings);
    await waitFor(() => screen.getByTestId("agent-access-clients"));

    expect(container.textContent).not.toContain(TOKEN);
    for (const id of [
      "agent-access-claude-code",
      "agent-access-codex-env",
      "agent-access-claude-desktop",
    ]) {
      expect(screen.getByTestId(id)).toHaveTextContent("DEFG");
    }
    for (const id of [
      "agent-access-claude-code-copy",
      "agent-access-codex-env-copy",
      "agent-access-claude-desktop-copy",
    ]) {
      copyTextMock.mockClear();
      await fireEvent.click(screen.getByTestId(id));
      await waitFor(() =>
        expect(copyTextMock).toHaveBeenCalledWith(expect.stringContaining(TOKEN)),
      );
    }

    await fireEvent.click(screen.getByTestId("agent-access-token-reveal"));
    for (const id of [
      "agent-access-claude-code",
      "agent-access-codex-env",
      "agent-access-claude-desktop",
    ]) {
      expect(screen.getByTestId(id)).toHaveTextContent(TOKEN);
    }
  });

  it("shows a failed action and keeps the last known state", async () => {
    backend = listening();
    render(AgentAccessSettings);
    await waitFor(() => screen.getByTestId("agent-access-toggle"));
    invokeMock.mockImplementationOnce(async () => {
      throw new Error("could not save");
    });
    await fireEvent.click(screen.getByTestId("agent-access-toggle"));
    await waitFor(() =>
      expect(screen.getByTestId("agent-access-action-error")).toHaveTextContent("could not save"),
    );
    expect(screen.getByTestId("agent-access-toggle")).toHaveAttribute("aria-checked", "true");
  });

  it("says so when the setting cannot be read", async () => {
    invokeMock.mockImplementation(async () => {
      throw new Error("no state");
    });
    render(AgentAccessSettings);
    await waitFor(() =>
      expect(screen.getByTestId("agent-access-load-error")).toHaveTextContent("no state"),
    );
  });
});
