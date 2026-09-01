import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { Preferences } from "$lib/types";
import { DEFAULT_AGENT_PROFILES } from "$lib/agentSelection";

// Each test controls the `get_preferences` / `set_preferences` responses.
const invokeMock = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: Record<string, unknown>) => invokeMock(cmd, args),
}));

// Imported after the mock so the store's `api` calls route through it.
const { preferences, saveStatus, loadPreferences, updatePreferences, _testing } =
  await import("./preferences.svelte");

afterEach(() => {
  _testing.reset({ ready: false });
  invokeMock.mockReset();
});

const PREFS = (editor: string | null, terminal: string): Preferences => ({
  editor_command: editor,
  terminal_app: terminal,
  diff_style: "side_by_side",
  show_builtins: true,
  claude_chrome_enabled: false,
  auto_reading_mode: false,
  notify_on_completion: true,
  notify_while_focused: false,
  agent_defaults: structuredClone(DEFAULT_AGENT_PROFILES),
});

beforeEach(() => {
  _testing.reset({ ready: true });
});

describe("preferences store", () => {
  it("loads backend values into the store", async () => {
    _testing.reset({ ready: false });
    invokeMock.mockResolvedValueOnce(PREFS("zed", "iTerm"));
    await loadPreferences();
    expect(preferences.editor_command).toBe("zed");
    expect(preferences.terminal_app).toBe("iTerm");
  });

  it("an edit waits for the authoritative load and preserves untouched fields", async () => {
    _testing.reset({ ready: false });
    // get_preferences resolves only when we release it — simulating a slow load.
    let releaseLoad!: (p: Preferences) => void;
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "get_preferences") {
        return new Promise<Preferences>((resolve) => {
          releaseLoad = resolve;
        });
      }
      return Promise.resolve(null); // set_preferences
    });

    const loadPromise = loadPreferences();
    const updatePromise = updatePreferences({ editor_command: "cursor" });
    await vi.waitFor(() => expect(invokeMock).toHaveBeenCalledTimes(1));

    releaseLoad(PREFS("old-editor", "iTerm"));
    await Promise.all([loadPromise, updatePromise]);

    expect(preferences.editor_command).toBe("cursor");
    expect(preferences.terminal_app).toBe("iTerm");
    expect(invokeMock).toHaveBeenLastCalledWith("set_preferences", {
      preferences: expect.objectContaining({ editor_command: "cursor", terminal_app: "iTerm" }),
    });
  });

  it("a failed save sets saveStatus.error but keeps the in-memory value", async () => {
    invokeMock.mockRejectedValueOnce(new Error("disk full")); // set_preferences
    await updatePreferences({ editor_command: "cursor" });
    expect(saveStatus.error).toContain("disk full");
    expect(preferences.editor_command).toBe("cursor");
  });

  it("a subsequent successful save clears the error", async () => {
    invokeMock.mockRejectedValueOnce(new Error("disk full"));
    await updatePreferences({ editor_command: "cursor" });
    expect(saveStatus.error).not.toBeNull();

    invokeMock.mockResolvedValueOnce(null);
    await updatePreferences({ terminal_app: "iTerm" });
    expect(saveStatus.error).toBeNull();
  });

  it("serializes rapid whole-object saves so an older write cannot land last", async () => {
    const releases: Array<() => void> = [];
    invokeMock.mockImplementation(
      () =>
        new Promise<void>((resolve) => {
          releases.push(resolve);
        }),
    );

    const first = updatePreferences({ editor_command: "cursor" });
    const second = updatePreferences({ terminal_app: "iTerm" });
    await vi.waitFor(() => expect(invokeMock).toHaveBeenCalledTimes(1));

    releases[0]?.();
    await first;
    await vi.waitFor(() => expect(invokeMock).toHaveBeenCalledTimes(2));
    expect(invokeMock).toHaveBeenLastCalledWith("set_preferences", {
      preferences: expect.objectContaining({ editor_command: "cursor", terminal_app: "iTerm" }),
    });
    releases[1]?.();
    await second;
    // The store is optimistic regardless of persistence latency.
    expect(preferences.editor_command).toBe("cursor");
    expect(preferences.terminal_app).toBe("iTerm");
  });

  it("loads and persists the show_builtins toggle", async () => {
    _testing.reset({ ready: false });
    invokeMock.mockResolvedValueOnce({ ...PREFS("code", "Terminal"), show_builtins: false });
    await loadPreferences();
    expect(preferences.show_builtins).toBe(false);

    invokeMock.mockResolvedValueOnce(null);
    await updatePreferences({ show_builtins: true });
    expect(preferences.show_builtins).toBe(true);
    // The whole merged value is sent to the backend.
    expect(invokeMock).toHaveBeenLastCalledWith("set_preferences", {
      preferences: expect.objectContaining({ show_builtins: true }),
    });
  });

  it("loads and persists the claude_chrome_enabled toggle", async () => {
    _testing.reset({ ready: false });
    invokeMock.mockResolvedValueOnce({
      ...PREFS("code", "Terminal"),
      claude_chrome_enabled: true,
    });
    await loadPreferences();
    expect(preferences.claude_chrome_enabled).toBe(true);

    invokeMock.mockResolvedValueOnce(null);
    await updatePreferences({ claude_chrome_enabled: false });
    expect(preferences.claude_chrome_enabled).toBe(false);
    expect(invokeMock).toHaveBeenLastCalledWith("set_preferences", {
      preferences: expect.objectContaining({ claude_chrome_enabled: false }),
    });
  });

  it("settles readiness with built-in defaults when loading fails", async () => {
    _testing.reset({ ready: false });
    invokeMock.mockRejectedValueOnce(new Error("config unreadable"));

    await expect(loadPreferences()).resolves.toBeUndefined();

    expect(preferences.editor_command).toBe("code");
    invokeMock.mockResolvedValueOnce(null);
    await updatePreferences({ editor_command: "cursor" });
    expect(invokeMock).toHaveBeenCalledTimes(2);
  });
});
