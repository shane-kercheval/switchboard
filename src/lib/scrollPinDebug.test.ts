import { afterEach, describe, expect, it, vi } from "vitest";
import { attachPointerProbe, debugInput, debugSample } from "./scrollPinDebug";

// The contract that matters for a diagnostic living permanently in the tree:
// with the flag unset it must be inert — no output, no listeners, no throw —
// so shipping it costs nothing but an unused import.

afterEach(() => {
  vi.restoreAllMocks();
});

describe("disabled by default", () => {
  it("prints nothing and attaches nothing", () => {
    const log = vi.spyOn(console, "log").mockImplementation(() => undefined);
    const el = document.createElement("div");
    const addListener = vi.spyOn(el, "addEventListener");

    debugSample("outer", "up", { scrollTop: 10, scrollHeight: 100, clientHeight: 50 }, true, false);
    debugInput("outer", "wheel-down", 42);
    const detach = attachPointerProbe(el);

    expect(log).not.toHaveBeenCalled();
    expect(addListener).not.toHaveBeenCalled();
    expect(detach).toBeUndefined();
  });
});

describe("console handle", () => {
  it("exposes enable/disable/summary/reset without a build step", () => {
    const handle = globalThis.switchboardPinDebug;
    expect(handle).toBeDefined();
    expect(typeof handle?.enable).toBe("function");
    expect(typeof handle?.disable).toBe("function");
    expect(typeof handle?.verbose).toBe("function");
    expect(typeof handle?.tail).toBe("function");
    expect(typeof handle?.summary).toBe("function");
    expect(typeof handle?.reset).toBe("function");
  });
});
