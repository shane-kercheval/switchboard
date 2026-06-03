import { beforeEach, describe, expect, it, vi } from "vitest";
import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/svelte";
import McpServersSettings from "./McpServersSettings.svelte";
import type { McpProviderInfo } from "$lib/types";

const invokeMock = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: Record<string, unknown>) => invokeMock(cmd, args),
}));

// Capture event listeners so a test can fire the backend's `prompts:synced`
// signal and assert the component re-refreshes.
const eventListeners = new Map<string, (e: { payload: unknown }) => void>();
vi.mock("@tauri-apps/api/event", () => ({
  listen: (event: string, handler: (e: { payload: unknown }) => void) => {
    eventListeners.set(event, handler);
    return Promise.resolve(() => eventListeners.delete(event));
  },
}));

// A mutable fake backend so add/remove reflect in the next list fetch.
let providers: McpProviderInfo[];

beforeEach(() => {
  providers = [];
  eventListeners.clear();
  invokeMock.mockReset();
  invokeMock.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
    switch (cmd) {
      case "list_mcp_providers":
        return providers;
      case "add_mcp_provider":
        providers = [
          ...providers,
          {
            name: args?.name as string,
            url: args?.url as string,
            has_token: args?.bearer !== null,
            status: { state: "unknown" },
          },
        ];
        return null;
      case "remove_mcp_provider":
        providers = providers.filter((p) => p.name !== (args?.name as string));
        return null;
      case "test_mcp_connection":
        return 3;
      case "sync_prompts":
        return null;
      default:
        throw new Error(`unexpected invoke: ${cmd}`);
    }
  });
});

describe("McpServersSettings", () => {
  it("lists configured providers with status on mount", async () => {
    providers = [
      {
        name: "team",
        url: "https://x/mcp",
        has_token: true,
        status: { state: "ok", prompt_count: 2 },
      },
    ];
    render(McpServersSettings);
    await waitFor(() => expect(screen.getByTestId("mcp-row-team")).toBeInTheDocument());
    expect(screen.getByTestId("mcp-status-team")).toHaveTextContent("2 prompts");
  });

  it("flags a missing token", async () => {
    providers = [
      { name: "team", url: "https://x", has_token: false, status: { state: "unknown" } },
    ];
    render(McpServersSettings);
    await waitFor(() => expect(screen.getByTestId("mcp-row-team")).toBeInTheDocument());
    expect(screen.getByTestId("mcp-row-team")).toHaveTextContent("no token");
  });

  it("rejects the reserved name `local` and blocks submit", async () => {
    render(McpServersSettings);
    await waitFor(() => expect(screen.getByTestId("mcp-empty")).toBeInTheDocument());
    await fireEvent.input(screen.getByTestId("mcp-name"), { target: { value: "local" } });
    await fireEvent.input(screen.getByTestId("mcp-url"), { target: { value: "https://x" } });
    expect(screen.getByTestId("mcp-name-error")).toBeInTheDocument();
    expect((screen.getByTestId("mcp-add") as HTMLButtonElement).disabled).toBe(true);
  });

  it("adds a provider (bearer null when blank) and refreshes the list", async () => {
    render(McpServersSettings);
    await waitFor(() => expect(screen.getByTestId("mcp-empty")).toBeInTheDocument());
    await fireEvent.input(screen.getByTestId("mcp-name"), { target: { value: "team" } });
    await fireEvent.input(screen.getByTestId("mcp-url"), { target: { value: "https://x/mcp" } });
    await fireEvent.click(screen.getByTestId("mcp-add"));
    await waitFor(() => expect(screen.getByTestId("mcp-row-team")).toBeInTheDocument());
    const addCall = invokeMock.mock.calls.find(([c]) => c === "add_mcp_provider");
    expect(addCall?.[1]).toMatchObject({ name: "team", url: "https://x/mcp", bearer: null });
  });

  it("sends the bearer when provided", async () => {
    render(McpServersSettings);
    await waitFor(() => expect(screen.getByTestId("mcp-empty")).toBeInTheDocument());
    await fireEvent.input(screen.getByTestId("mcp-name"), { target: { value: "team" } });
    await fireEvent.input(screen.getByTestId("mcp-url"), { target: { value: "https://x" } });
    await fireEvent.input(screen.getByTestId("mcp-bearer"), { target: { value: "tok" } });
    await fireEvent.click(screen.getByTestId("mcp-add"));
    await waitFor(() => {
      const addCall = invokeMock.mock.calls.find(([c]) => c === "add_mcp_provider");
      expect(addCall?.[1]).toMatchObject({ name: "team", url: "https://x", bearer: "tok" });
    });
  });

  it("removes a provider", async () => {
    providers = [
      { name: "team", url: "https://x", has_token: false, status: { state: "unknown" } },
    ];
    render(McpServersSettings);
    await waitFor(() => expect(screen.getByTestId("mcp-row-team")).toBeInTheDocument());
    await fireEvent.click(screen.getByTestId("mcp-remove-team"));
    await waitFor(() => expect(screen.queryByTestId("mcp-row-team")).not.toBeInTheDocument());
    expect(invokeMock).toHaveBeenCalledWith("remove_mcp_provider", { name: "team" });
  });

  it("refreshes a just-added provider's status on the prompts:synced event", async () => {
    providers = [
      { name: "team", url: "https://x", has_token: false, status: { state: "unknown" } },
    ];
    render(McpServersSettings);
    await waitFor(() => expect(screen.getByTestId("mcp-status-team")).toBeInTheDocument());
    expect(screen.getByTestId("mcp-status-team")).not.toHaveTextContent("2 prompts");

    // Background sync completes: backend now reports a real status.
    providers = [
      {
        name: "team",
        url: "https://x",
        has_token: false,
        status: { state: "ok", prompt_count: 2 },
      },
    ];
    eventListeners.get("prompts:synced")?.({ payload: null });

    await waitFor(() =>
      expect(screen.getByTestId("mcp-status-team")).toHaveTextContent("2 prompts"),
    );
  });

  it("test connection reports the prompt count", async () => {
    render(McpServersSettings);
    await waitFor(() => expect(screen.getByTestId("mcp-empty")).toBeInTheDocument());
    await fireEvent.input(screen.getByTestId("mcp-url"), { target: { value: "https://x/mcp" } });
    await fireEvent.click(screen.getByTestId("mcp-test"));
    await waitFor(() =>
      expect(screen.getByTestId("mcp-test-result")).toHaveTextContent("3 prompts"),
    );
  });
});
