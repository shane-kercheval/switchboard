import { beforeEach, describe, expect, it, vi } from "vitest";

async function loadStore(): Promise<typeof import("./agentCopy.svelte")> {
  vi.resetModules();
  return import("./agentCopy.svelte");
}

beforeEach(() => {
  localStorage.clear();
});

describe("agent copy preference", () => {
  it("defaults to last answer block when nothing is stored", async () => {
    const { agentCopy } = await loadStore();
    expect(agentCopy.mode).toBe("last_answer_block");
  });

  it("reads a persisted mode on load", async () => {
    localStorage.setItem("switchboard-agent-copy-mode", "full_answer");
    const { agentCopy } = await loadStore();
    expect(agentCopy.mode).toBe("full_answer");
  });

  it("persists mode changes", async () => {
    const { agentCopy } = await loadStore();
    agentCopy.set("full_answer");
    expect(localStorage.getItem("switchboard-agent-copy-mode")).toBe("full_answer");
    expect(agentCopy.mode).toBe("full_answer");
  });

  it("falls back to last answer block for unknown persisted values", async () => {
    localStorage.setItem("switchboard-agent-copy-mode", "bogus");
    const { agentCopy } = await loadStore();
    expect(agentCopy.mode).toBe("last_answer_block");
  });
});
