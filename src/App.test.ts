import { vi, describe, it, expect, beforeEach } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, screen, waitFor } from "@testing-library/svelte";

const invokeMock = vi.fn(
  async (_cmd: string, _args?: Record<string, unknown>): Promise<unknown> => null,
);
const listenMock = vi.fn(
  async (_event: string, _handler: unknown): Promise<() => void> =>
    () => {},
);
const openDialogMock = vi.fn(async (_options: unknown): Promise<string | null> => null);

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: Record<string, unknown>) => invokeMock(cmd, args),
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: (event: string, handler: unknown) => listenMock(event, handler),
}));

vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: (options: unknown) => openDialogMock(options),
}));

describe("App", () => {
  beforeEach(() => {
    invokeMock.mockReset();
    listenMock.mockReset();
    openDialogMock.mockReset();
  });

  it("mounts and renders the welcome screen when the binary check succeeds", async () => {
    invokeMock.mockResolvedValueOnce(null);
    const App = (await import("./App.svelte")).default;
    render(App);
    expect(screen.getByText("Switchboard")).toBeInTheDocument();
    expect(screen.getByText("Open working directory")).toBeInTheDocument();
    await waitFor(() => {
      expect(screen.queryByTestId("binary-not-found-banner")).not.toBeInTheDocument();
    });
  });

  it("renders the binary-not-found banner when the startup probe fails", async () => {
    invokeMock.mockRejectedValueOnce(new Error("harness probe failed: harness binary not found"));
    const App = (await import("./App.svelte")).default;
    render(App);
    await waitFor(() => {
      expect(screen.getByTestId("binary-not-found-banner")).toBeInTheDocument();
    });
  });
});
