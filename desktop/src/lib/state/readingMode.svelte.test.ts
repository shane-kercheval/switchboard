import { afterEach, describe, expect, it, vi } from "vitest";
import {
  clearReadingMode,
  enterReadingMode,
  forgetReadingMode,
  isReadingMode,
  toggleReadingMode,
  _testing,
} from "./readingMode.svelte";

afterEach(() => _testing.reset());

describe("reading mode state", () => {
  it("is per project", () => {
    expect(toggleReadingMode("p-a")).toBe(true);
    expect(isReadingMode("p-a")).toBe(true);
    expect(isReadingMode("p-b")).toBe(false);

    expect(toggleReadingMode("p-b")).toBe(true);
    expect(toggleReadingMode("p-a")).toBe(false);
    expect(isReadingMode("p-a")).toBe(false);
    expect(isReadingMode("p-b")).toBe(true);
  });

  it("enters idempotently — a send while the mode is already on must leave it on", () => {
    enterReadingMode("p-a");
    enterReadingMode("p-a");
    expect(isReadingMode("p-a")).toBe(true);
    // A manual exit still works afterwards.
    expect(toggleReadingMode("p-a")).toBe(false);
    expect(isReadingMode("p-a")).toBe(false);
  });

  it("clears idempotently, since the flush clears on every project quiet-down", () => {
    toggleReadingMode("p-a");
    clearReadingMode("p-a");
    clearReadingMode("p-a");
    expect(isReadingMode("p-a")).toBe(false);
  });

  it("forgets removed projects without touching the survivors", () => {
    toggleReadingMode("p-a");
    toggleReadingMode("p-b");
    forgetReadingMode(["p-a"]);
    expect(isReadingMode("p-a")).toBe(false);
    expect(isReadingMode("p-b")).toBe(true);
  });

  it("does not survive a reload", async () => {
    toggleReadingMode("p-a");
    // A reload re-evaluates the module against a clean heap; a fresh module
    // registry is the closest jsdom equivalent. If reading mode ever grew a
    // persisted backing store (localStorage, a preference), the reloaded module
    // would read the flag back here and this would fail — which is the point.
    vi.resetModules();
    const reloaded = await import("./readingMode.svelte");
    expect(reloaded.isReadingMode("p-a")).toBe(false);
  });
});
