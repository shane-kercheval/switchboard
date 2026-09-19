import { beforeEach, describe, expect, it, vi } from "vitest";

const invokeMock = vi.fn<(cmd: string, args?: Record<string, unknown>) => Promise<unknown>>();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: Record<string, unknown>) => invokeMock(cmd, args),
}));

const usage = await import("./harnessUsage.svelte");

/// Two readings of the same weekly window, and one of a window that has since
/// rolled. Window identity is the set of reset times, so a percentage may move
/// within one window without making it a different window.
///
/// Payloads are compared with `toEqual` rather than `toBe`: the store is
/// `$state`, which deep-proxies whatever is written into it, so object identity
/// never survives the write.
const WEEKLY = { primary: { used_percent: 93, window_minutes: 10080, resets_at: 1_789_845_487 } };
const WEEKLY_LATER = {
  primary: { used_percent: 96, window_minutes: 10080, resets_at: 1_789_845_487 },
};
const ROLLED = { primary: { used_percent: 2, window_minutes: 10080, resets_at: 1_790_375_461 } };

beforeEach(() => {
  invokeMock.mockReset();
  invokeMock.mockResolvedValue(undefined);
  usage._testing.reset();
});

describe("observeUsage", () => {
  it("keeps the newer reading whichever order the two arrive in", () => {
    // Order-independence is the whole point: agents on different projects report
    // the same account's quota whenever each of them last ran, and a project
    // opened later can easily carry an older reading than the one in memory.
    usage.observeUsage("codex", { payload: WEEKLY, observed_at: "2026-09-18T20:00:00Z" });
    usage.observeUsage("codex", { payload: ROLLED, observed_at: "2026-09-18T19:00:00Z" });
    expect(usage.harnessUsage.codex?.payload).toEqual(WEEKLY);

    usage._testing.reset();
    usage.observeUsage("codex", { payload: ROLLED, observed_at: "2026-09-18T19:00:00Z" });
    usage.observeUsage("codex", { payload: WEEKLY, observed_at: "2026-09-18T20:00:00Z" });
    expect(usage.harnessUsage.codex?.payload).toEqual(WEEKLY);
  });

  it("keeps each harness independent", () => {
    usage.observeUsage("codex", { payload: WEEKLY, observed_at: "2026-09-18T20:00:00Z" });
    usage.observeUsage("claude_code", { payload: ROLLED, observed_at: "2026-09-18T19:00:00Z" });
    expect(usage.harnessUsage.codex?.payload).toEqual(WEEKLY);
    expect(usage.harnessUsage.claude_code?.payload).toEqual(ROLLED);
  });

  it("takes the whole reading rather than merging windows into the stored one", () => {
    // Two weekly windows with different resets can coexist across agents after a
    // plan change. Merging per label would flip between them; a whole snapshot
    // cannot describe a state no agent reported.
    usage.observeUsage("codex", {
      payload: WEEKLY,
      observed_at: "2026-09-18T20:00:00Z",
      model: "gpt-5.6-sol",
    });
    usage.observeUsage("codex", { payload: ROLLED, observed_at: "2026-09-18T21:00:00Z" });
    expect(usage.harnessUsage.codex).toEqual({
      payload: ROLLED,
      observed_at: "2026-09-18T21:00:00Z",
    });
  });

  it("persists the whole map on every change", async () => {
    usage.observeUsage("codex", { payload: WEEKLY, observed_at: "2026-09-18T20:00:00Z" });
    await vi.waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("set_harness_usage", {
        usage: { harnesses: { codex: { payload: WEEKLY, observed_at: "2026-09-18T20:00:00Z" } } },
      }),
    );
  });

  it("does not persist a reading it discarded", () => {
    usage.observeUsage("codex", { payload: WEEKLY, observed_at: "2026-09-18T20:00:00Z" });
    invokeMock.mockClear();
    usage.observeUsage("codex", { payload: ROLLED, observed_at: "2026-09-18T19:00:00Z" });
    expect(invokeMock).not.toHaveBeenCalled();
  });
});

describe("the refusal verdict", () => {
  function refusedOnWeekly(): void {
    usage.observeUsage("codex", { payload: WEEKLY, observed_at: "2026-09-18T20:00:00Z" });
    usage.recordUsageRefusal("codex");
  }

  it("survives a later reading of the same window", () => {
    // The refused turn's own reading re-reports the window it was refused on, so
    // the verdict has to outlive the sequence that set it.
    refusedOnWeekly();
    usage.observeUsage("codex", { payload: WEEKLY_LATER, observed_at: "2026-09-18T20:05:00Z" });
    expect(usage.harnessUsage.codex?.limit_reached).toBe(true);
  });

  it("retires with the window it judged", () => {
    refusedOnWeekly();
    usage.observeUsage("codex", { payload: ROLLED, observed_at: "2026-09-25T09:00:00Z" });
    expect(usage.harnessUsage.codex?.limit_reached).toBeUndefined();
  });

  it("is cleared by a completed turn and by nothing else", () => {
    refusedOnWeekly();
    usage.clearUsageRefusal("codex");
    expect(usage.harnessUsage.codex?.limit_reached).toBeUndefined();
  });

  it("attaches to nothing when no reading is held", () => {
    // A verdict is about a reading; with no measurement there is no window to
    // mark, and inventing an entry would render a card with no meters.
    usage.recordUsageRefusal("codex");
    expect(usage.harnessUsage.codex).toBeUndefined();
  });

  it("does not re-persist when the verdict is already what it would be set to", () => {
    refusedOnWeekly();
    invokeMock.mockClear();
    usage.recordUsageRefusal("codex");
    expect(invokeMock).not.toHaveBeenCalled();
  });
});

describe("loadPersistedUsage", () => {
  it("ranks the restored reading rather than applying it over a live one", async () => {
    usage.observeUsage("codex", { payload: WEEKLY_LATER, observed_at: "2026-09-18T20:05:00Z" });
    invokeMock.mockImplementation(async (cmd) =>
      cmd === "get_harness_usage"
        ? { harnesses: { codex: { payload: ROLLED, observed_at: "2026-09-18T19:00:00Z" } } }
        : undefined,
    );
    await usage.loadPersistedUsage();
    expect(usage.harnessUsage.codex?.payload).toEqual(WEEKLY_LATER);
  });

  it("restores a reading when memory has none", async () => {
    invokeMock.mockImplementation(async (cmd) =>
      cmd === "get_harness_usage"
        ? {
            harnesses: {
              codex: {
                payload: WEEKLY,
                observed_at: "2026-09-18T19:00:00Z",
                limit_reached: true,
                model: "gpt-5.6-sol",
              },
            },
          }
        : undefined,
    );
    await usage.loadPersistedUsage();
    expect(usage.harnessUsage.codex).toEqual({
      payload: WEEKLY,
      observed_at: "2026-09-18T19:00:00Z",
      limit_reached: true,
      model: "gpt-5.6-sol",
    });
  });

  it("keeps an entry with no observation time, ranked below anything stamped", async () => {
    // Undated is a shape this module writes itself, so it round-trips rather than
    // being discarded, and it loses to every stamped reading.
    invokeMock.mockImplementation(async (cmd) =>
      cmd === "get_harness_usage" ? { harnesses: { codex: { payload: WEEKLY } } } : undefined,
    );
    await usage.loadPersistedUsage();
    expect(usage.harnessUsage.codex?.payload).toEqual(WEEKLY);
    expect(usage.harnessUsage.codex?.observed_at).toBeUndefined();

    usage.observeUsage("codex", { payload: ROLLED, observed_at: "2026-09-18T19:00:00Z" });
    expect(usage.harnessUsage.codex?.payload).toEqual(ROLLED);
  });

  it.each([
    ["a non-string observation time", { payload: WEEKLY, observed_at: 1_789_845_487 }],
    ["no payload", { observed_at: "2026-09-18T19:00:00Z" }],
    ["a non-object entry", "nope"],
    ["null", null],
  ])("drops an entry with %s", async (_case, entry) => {
    invokeMock.mockImplementation(async (cmd) =>
      cmd === "get_harness_usage" ? { harnesses: { codex: entry } } : undefined,
    );
    await usage.loadPersistedUsage();
    expect(usage.harnessUsage.codex).toBeUndefined();
  });

  it("survives a failed read with no readings rather than throwing", async () => {
    invokeMock.mockRejectedValue(new Error("unreadable"));
    await expect(usage.loadPersistedUsage()).resolves.toBeUndefined();
    expect(usage.harnessUsage.codex).toBeUndefined();
  });
});

describe("nameUsageModel", () => {
  it("labels a reading that arrived before its turn's init", () => {
    usage.observeUsage("claude_code", { payload: WEEKLY, observed_at: "2026-09-18T20:00:00Z" });
    usage.nameUsageModel("claude_code", "claude-fable-5-1");
    expect(usage.harnessUsage.claude_code?.model).toBe("claude-fable-5-1");
  });

  it("never relabels a reading that already names a model", () => {
    // A later turn on a different model must not rewrite the label on a window
    // the earlier model delivered.
    usage.observeUsage("claude_code", {
      payload: WEEKLY,
      observed_at: "2026-09-18T20:00:00Z",
      model: "claude-fable-5-1",
    });
    usage.nameUsageModel("claude_code", "claude-sonnet-5");
    expect(usage.harnessUsage.claude_code?.model).toBe("claude-fable-5-1");
  });

  it.each([
    ["undefined", undefined],
    ["an empty string", ""],
  ])("ignores %s rather than storing it as a label", (_case, model) => {
    usage.observeUsage("claude_code", { payload: WEEKLY, observed_at: "2026-09-18T20:00:00Z" });
    usage.nameUsageModel("claude_code", model);
    expect(usage.harnessUsage.claude_code?.model).toBeUndefined();
  });

  it("does nothing when no reading is held", () => {
    usage.nameUsageModel("claude_code", "claude-fable-5-1");
    expect(usage.harnessUsage.claude_code).toBeUndefined();
  });
});

describe("ordering readings by instant rather than by text", () => {
  /// The producers disagree on fractional precision and both shapes reach one
  /// file: chrono emits six digits or none, `toISOString` always emits three.
  /// Compared as text, a digit sorts below `Z`, so the more precise stamp loses.
  it("prefers the later reading when precision differs within a second", () => {
    usage.observeUsage("codex", { payload: WEEKLY, observed_at: "2026-09-19T03:51:13.898649Z" });
    usage.observeUsage("codex", { payload: ROLLED, observed_at: "2026-09-19T03:51:13.899Z" });
    expect(usage.harnessUsage.codex?.payload).toEqual(ROLLED);
  });

  it("keeps the held reading when the coarser stamp is the older one", () => {
    usage.observeUsage("codex", { payload: WEEKLY, observed_at: "2026-09-19T03:51:13.899Z" });
    usage.observeUsage("codex", { payload: ROLLED, observed_at: "2026-09-19T03:51:13.898649Z" });
    expect(usage.harnessUsage.codex?.payload).toEqual(WEEKLY);
  });

  it("prefers a sub-second stamp over a whole-second one in the same second", () => {
    // The `Z`-versus-`.` case: lexically the whole second wins, which is wrong.
    usage.observeUsage("codex", { payload: WEEKLY, observed_at: "2026-09-19T03:51:13Z" });
    usage.observeUsage("codex", { payload: ROLLED, observed_at: "2026-09-19T03:51:13.500Z" });
    expect(usage.harnessUsage.codex?.payload).toEqual(ROLLED);
  });

  it("lets any stamped reading supersede an undated one", () => {
    usage.observeUsage("codex", { payload: WEEKLY });
    usage.observeUsage("codex", { payload: ROLLED, observed_at: "1999-01-01T00:00:00Z" });
    expect(usage.harnessUsage.codex?.payload).toEqual(ROLLED);
  });

  it("never lets an undated reading supersede a stamped one", () => {
    usage.observeUsage("codex", { payload: WEEKLY, observed_at: "1999-01-01T00:00:00Z" });
    usage.observeUsage("codex", { payload: ROLLED });
    expect(usage.harnessUsage.codex?.payload).toEqual(WEEKLY);
  });

  it("keeps the first of two undated readings, since neither can claim to be later", () => {
    usage.observeUsage("codex", { payload: WEEKLY });
    usage.observeUsage("codex", { payload: ROLLED });
    expect(usage.harnessUsage.codex?.payload).toEqual(WEEKLY);
  });

  it("treats an unparseable instant as undated rather than trusting it", () => {
    usage.observeUsage("codex", { payload: WEEKLY, observed_at: "not a date" });
    usage.observeUsage("codex", { payload: ROLLED, observed_at: "1999-01-01T00:00:00Z" });
    expect(usage.harnessUsage.codex?.payload).toEqual(ROLLED);
  });
});

describe("writing the file", () => {
  /// A write that never settles until released, so a burst can be observed while
  /// the first one is still in flight.
  function heldWrite(): { release: () => void; calls: () => number } {
    let release!: () => void;
    const gate = new Promise<void>((r) => (release = r));
    let calls = 0;
    invokeMock.mockImplementation(async (cmd) => {
      if (cmd !== "set_harness_usage") return undefined;
      calls += 1;
      await gate;
      return undefined;
    });
    return { release, calls: () => calls };
  }

  it("collapses a burst into one further write rather than one per change", async () => {
    const write = heldWrite();
    usage.observeUsage("codex", { payload: WEEKLY, observed_at: "2026-09-18T20:00:00Z" });
    await vi.waitFor(() => expect(write.calls()).toBe(1));

    // Three more changes while the first write is still in flight.
    usage.observeUsage("codex", { payload: WEEKLY_LATER, observed_at: "2026-09-18T20:01:00Z" });
    usage.recordUsageRefusal("codex");
    usage.clearUsageRefusal("codex");
    expect(write.calls()).toBe(1);

    write.release();
    await vi.waitFor(() => expect(write.calls()).toBe(2));
  });

  it("writes the map as it stands when the write runs, not when it was requested", async () => {
    const write = heldWrite();
    usage.observeUsage("codex", { payload: WEEKLY, observed_at: "2026-09-18T20:00:00Z" });
    await vi.waitFor(() => expect(write.calls()).toBe(1));
    usage.observeUsage("codex", { payload: ROLLED, observed_at: "2026-09-18T21:00:00Z" });
    write.release();

    await vi.waitFor(() => expect(write.calls()).toBe(2));
    const last = invokeMock.mock.calls.at(-1);
    expect(
      (last?.[1] as { usage: { harnesses: { codex: { payload: unknown } } } }).usage.harnesses.codex
        .payload,
    ).toEqual(ROLLED);
  });

  it("keeps writing after a failed write rather than wedging the file", async () => {
    // A rejected write must not leave the in-flight marker set, or every later
    // change would be silently dropped for the life of the session.
    invokeMock.mockRejectedValueOnce(new Error("disk full"));
    usage.observeUsage("codex", { payload: WEEKLY, observed_at: "2026-09-18T20:00:00Z" });
    await vi.waitFor(() =>
      expect(invokeMock.mock.calls.filter((c) => c[0] === "set_harness_usage")).toHaveLength(1),
    );

    invokeMock.mockResolvedValue(undefined);
    usage.observeUsage("codex", { payload: ROLLED, observed_at: "2026-09-18T21:00:00Z" });
    await vi.waitFor(() =>
      expect(invokeMock.mock.calls.filter((c) => c[0] === "set_harness_usage")).toHaveLength(2),
    );
  });
});
