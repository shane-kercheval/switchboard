import { afterEach, describe, expect, it, vi } from "vitest";
import type { HarnessKind } from "./types";

const invokeMock = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: Record<string, unknown>) => invokeMock(cmd, args),
}));

import {
  _testing,
  harnessAvailability,
  refreshHarnessAvailability,
} from "./harnessAvailability.svelte";

const ALL: HarnessKind[] = ["claude_code", "codex", "gemini", "antigravity"];

afterEach(() => {
  _testing.reset();
  invokeMock.mockReset();
});

describe("harnessAvailability store", () => {
  it("derives 'checking' for every harness before any probe (fail-closed)", () => {
    for (const harness of ALL) {
      expect(harnessAvailability.status(harness)).toBeNull();
      expect(harnessAvailability.availability(harness).binary).toBe("checking");
    }
    expect(harnessAvailability.installed()).toEqual([]);
  });

  it("populates all four entries on refresh", async () => {
    invokeMock.mockImplementation((_cmd: string, args?: Record<string, unknown>) => {
      const harness = args?.harness as HarnessKind;
      return Promise.resolve({ installed: true, version: `v-${harness}` });
    });
    await refreshHarnessAvailability();
    for (const harness of ALL) {
      expect(harnessAvailability.status(harness)).toEqual({
        installed: true,
        version: `v-${harness}`,
      });
      expect(harnessAvailability.availability(harness).binary).toBe("available");
    }
    expect(harnessAvailability.installed()).toEqual(ALL);
  });

  it("reflects a not-installed harness and excludes it from installed()", async () => {
    invokeMock.mockImplementation((_cmd: string, args?: Record<string, unknown>) => {
      const harness = args?.harness as HarnessKind;
      return Promise.resolve(
        harness === "gemini"
          ? { installed: false, version: null }
          : { installed: true, version: "1.0.0" },
      );
    });
    await refreshHarnessAvailability();
    expect(harnessAvailability.availability("gemini").binary).toBe("missing");
    // Order is deterministic by `HARNESSES` construction, not coincidence —
    // M4 auto-create relies on a stable iteration order.
    expect(harnessAvailability.installed()).toEqual(["claude_code", "codex", "antigravity"]);
  });

  it("degrades a rejected probe to not-installed rather than leaving it checking", async () => {
    invokeMock.mockImplementation((_cmd: string, args?: Record<string, unknown>) => {
      const harness = args?.harness as HarnessKind;
      return harness === "codex"
        ? Promise.reject(new Error("probe blew up"))
        : Promise.resolve({ installed: true, version: "1.0.0" });
    });
    await refreshHarnessAvailability();
    expect(harnessAvailability.status("codex")).toEqual({ installed: false, version: null });
    expect(harnessAvailability.availability("codex").binary).toBe("missing");
  });

  it("updates a previously-cached value on a later refresh", async () => {
    invokeMock.mockResolvedValue({ installed: false, version: null });
    await refreshHarnessAvailability();
    expect(harnessAvailability.availability("claude_code").binary).toBe("missing");

    invokeMock.mockResolvedValue({ installed: true, version: "2.0.0" });
    await refreshHarnessAvailability();
    expect(harnessAvailability.availability("claude_code").binary).toBe("available");
    expect(harnessAvailability.status("claude_code")).toEqual({
      installed: true,
      version: "2.0.0",
    });
  });
});
