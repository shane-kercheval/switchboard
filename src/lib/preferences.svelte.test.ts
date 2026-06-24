import { afterEach, describe, expect, it, vi } from "vitest";
import type { Preferences } from "$lib/types";

// Each test controls the `get_preferences` / `set_preferences` responses.
const invokeMock = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: Record<string, unknown>) => invokeMock(cmd, args),
}));

// Imported after the mock so the store's `api` calls route through it.
const { preferences, saveStatus, loadPreferences, updatePreferences, _testing } =
  await import("./preferences.svelte");

afterEach(() => {
  _testing.reset();
  invokeMock.mockReset();
});

const PREFS = (editor: string | null, terminal: string): Preferences => ({
  editor_command: editor,
  terminal_app: terminal,
  diff_style: "side_by_side",
  show_builtins: true,
});

describe("preferences store", () => {
  it("loads backend values into the store", async () => {
    invokeMock.mockResolvedValueOnce(PREFS("zed", "iTerm"));
    await loadPreferences();
    expect(preferences.editor_command).toBe("zed");
    expect(preferences.terminal_app).toBe("iTerm");
  });

  it("a user edit during an in-flight load is not clobbered by the late result", async () => {
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
    // User edits before the load resolves.
    await updatePreferences({ editor_command: "cursor" });
    // The late load arrives with the stale on-disk value.
    releaseLoad(PREFS("old-editor", "Terminal"));
    await loadPromise;

    expect(preferences.editor_command).toBe("cursor");
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

  it("loads and persists the show_builtins toggle", async () => {
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
});
