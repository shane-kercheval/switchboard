import { vi, describe, it, expect } from "vitest";
import { render, screen, waitFor } from "@testing-library/svelte";
import App from "./App.svelte";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (cmd: string, args: Record<string, unknown>) => {
    if (cmd === "ping") return `pong, ${args.name}`;
    throw new Error(`unexpected command: ${cmd}`);
  }),
}));

describe("App", () => {
  it("mounts and renders the title", () => {
    render(App);
    expect(screen.getByText("Switchboard")).toBeInTheDocument();
  });

  it("invokes ping on mount and renders the reply", async () => {
    render(App);
    await waitFor(() => {
      expect(screen.getByTestId("ping-reply")).toHaveTextContent("pong, world");
    });
  });
});
