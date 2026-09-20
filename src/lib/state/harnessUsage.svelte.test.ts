import { beforeEach, describe, expect, it, vi } from "vitest";

const invokeMock = vi.fn<(cmd: string, args?: Record<string, unknown>) => Promise<unknown>>();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: Record<string, unknown>) => invokeMock(cmd, args),
}));

const usage = await import("./harnessUsage.svelte");
const { claudeRateLimitView, claudeStoredWindows } = await import("$lib/usageWindows");

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

const WEEK_RESET = 1_789_845_487;
const NEXT_WEEK_RESET = 1_790_375_461;

/// Record a Claude reading the way the event path does: the payload alongside the
/// windows lifted out of it, which is what tells the store this reading is partial.
function observeClaude(
  windows: Record<string, unknown>,
  context: { observedAt?: string; model?: string; turnId?: string } = {},
  accountFields: Record<string, unknown> = {},
): void {
  const payload = { status: "allowed", ...accountFields, unifiedWindows: windows };
  usage.observeUsage("claude_code", {
    payload,
    observed_at: context.observedAt,
    windows: claudeStoredWindows(payload, context),
  });
}

/// The windows the store is holding for Claude, by key.
function heldWindows(): Record<
  string,
  { window: unknown; model?: string; observed_at?: string; turn_id?: string }
> {
  return usage.harnessUsage.claude_code?.windows ?? {};
}

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

  it("replaces a reading that names no windows of its own", () => {
    // A reading carrying no window map states every limit the account holds, so
    // nothing in the held one can still be true and unmentioned. Replacing is
    // correct there for exactly the reason merging is correct for Claude.
    usage.observeUsage("codex", { payload: WEEKLY, observed_at: "2026-09-18T20:00:00Z" });
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

describe("merging a reading that names its windows", () => {
  const FIVE_HOUR = { five_hour: { utilization: 0.07, resetsAt: WEEK_RESET } };
  const GATED = { seven_day_overage_included: { utilization: 1, resetsAt: WEEK_RESET } };

  it("keeps a window the new reading does not mention", () => {
    // The defect this exists for. A Claude reading omits the model-gated weekly
    // window unless the turn ran on a model it gates, so an Opus turn arriving
    // after a capped Fable turn used to delete a cap that was still blocking work.
    observeClaude(GATED, { observedAt: "2026-09-18T20:00:00Z", model: "claude-fable-5-1" });
    observeClaude(FIVE_HOUR, { observedAt: "2026-09-18T21:00:00Z", model: "claude-opus-5" });
    expect(Object.keys(heldWindows()).sort()).toEqual(["five_hour", "seven_day_overage_included"]);
    expect(heldWindows().seven_day_overage_included?.model).toBe("claude-fable-5-1");
  });

  it("updates a window the new reading does mention", () => {
    observeClaude(FIVE_HOUR, { observedAt: "2026-09-18T20:00:00Z" });
    observeClaude(
      { five_hour: { utilization: 0.4, resetsAt: WEEK_RESET } },
      { observedAt: "2026-09-18T21:00:00Z" },
    );
    expect(heldWindows().five_hour?.window).toEqual({ utilization: 0.4, resetsAt: WEEK_RESET });
  });

  it("keeps the held window when the arriving reading is older", () => {
    // Same ordering rule as the reading level, applied per key, so there is one
    // rule to know rather than two.
    observeClaude(FIVE_HOUR, { observedAt: "2026-09-18T21:00:00Z" });
    observeClaude(
      { five_hour: { utilization: 0.4, resetsAt: WEEK_RESET } },
      { observedAt: "2026-09-18T20:00:00Z" },
    );
    expect(heldWindows().five_hour?.window).toEqual({ utilization: 0.07, resetsAt: WEEK_RESET });
  });

  it("replaces a reissued window even from a reading that would lose on instant", () => {
    // A later reset is the vendor's own statement that this is a new generation, and
    // it outranks our measurement instant. Utilization only climbs *within* a
    // window, so retaining a value understates it and retention is safe; across a
    // reissue that invariant does not hold, and keeping the spent instance would
    // show a cap that has already cleared.
    observeClaude(
      { five_hour: { utilization: 1, resetsAt: WEEK_RESET } },
      { observedAt: "2026-09-18T21:00:00Z" },
    );
    observeClaude({ five_hour: { utilization: 0.02, resetsAt: NEXT_WEEK_RESET } });
    expect(heldWindows().five_hour?.window).toEqual({
      utilization: 0.02,
      resetsAt: NEXT_WEEK_RESET,
    });
  });

  it("skips a superseded instance rather than falling through to instant ranking", () => {
    // The reverse direction, and it must not merely lose on ranking: the held window
    // here is *undated*, so absent-ranks-last would hand the stale reading the win.
    // An earlier reset is positive evidence of the older generation.
    observeClaude({ five_hour: { utilization: 0.02, resetsAt: NEXT_WEEK_RESET } });
    observeClaude(
      { five_hour: { utilization: 1, resetsAt: WEEK_RESET } },
      { observedAt: "2026-09-18T21:00:00Z" },
    );
    expect(heldWindows().five_hour?.window).toEqual({
      utilization: 0.02,
      resetsAt: NEXT_WEEK_RESET,
    });
  });

  it("falls back to instant ranking when a reset cannot be read", () => {
    // An unreadable reset is no evidence either way, so the rule below it decides.
    observeClaude(
      { five_hour: { utilization: 0.02, resetsAt: "soon" } },
      { observedAt: "2026-09-18T20:00:00Z" },
    );
    observeClaude(
      { five_hour: { utilization: 0.5, resetsAt: WEEK_RESET } },
      { observedAt: "2026-09-18T21:00:00Z" },
    );
    expect(heldWindows().five_hour?.window).toEqual({ utilization: 0.5, resetsAt: WEEK_RESET });
  });

  it("lets an older reading contribute a window while the newer one keeps the account fields", () => {
    // Exactly a restored reading from the agent that ran the gated model: it is
    // authoritative about a window nothing else has seen and stale about the
    // account-level state, and those are different scopes.
    observeClaude(FIVE_HOUR, { observedAt: "2026-09-18T21:00:00Z" }, { isUsingOverage: true });
    observeClaude(GATED, { observedAt: "2026-09-18T20:00:00Z" }, { isUsingOverage: false });
    expect(Object.keys(heldWindows())).toContain("seven_day_overage_included");
    expect(usage.harnessUsage.claude_code?.payload).toMatchObject({ isUsingOverage: true });
  });

  it("does not let a project's stale snapshot take a live window off the card", () => {
    // The defect this ordering rule exists for, stated as the user's symptom. A
    // 5-hour window cycles every five hours, so any project sidecar older than that
    // carries a window that has since rolled. Written over the live one, its elapsed
    // reset then drops it at render — and with nothing else to show, the whole
    // Claude row disappears until the next turn.
    const now = Date.now();
    const live = Math.floor(now / 1000) + 3600;
    const cycled = Math.floor(now / 1000) - 4 * 3600;
    observeClaude(
      { five_hour: { utilization: 0.4, resetsAt: live } },
      { observedAt: new Date(now - 60_000).toISOString() },
    );
    observeClaude(
      { five_hour: { utilization: 0.9, resetsAt: cycled } },
      { observedAt: new Date(now - 6 * 3600_000).toISOString() },
    );
    const view = claudeRateLimitView(
      usage.harnessUsage.claude_code?.payload,
      usage.harnessUsage.claude_code?.windows,
      now,
    );
    expect(view?.windows.map((w) => w.key)).toEqual(["five_hour"]);
  });

  it("does not write the file for an older reading that adds no window", () => {
    observeClaude(FIVE_HOUR, { observedAt: "2026-09-18T21:00:00Z" });
    invokeMock.mockClear();
    observeClaude(
      { five_hour: { utilization: 0.4, resetsAt: WEEK_RESET } },
      { observedAt: "2026-09-18T20:00:00Z" },
    );
    expect(invokeMock).not.toHaveBeenCalled();
  });

  describe("reporting a reset that moved backward", () => {
    /// The premise the ordering rests on says a window's reset only advances, so a
    /// *newer* reading carrying an *earlier* reset cannot happen. It is unreachable by
    /// any test against the live CLI — seeing it needs two readings at least a window
    /// apart — so the running app is the only observer, and this is what it says.
    const LATER = "2026-09-18T21:00:00Z";
    const EARLIER = "2026-09-18T20:00:00Z";

    it("warns when the reset and the measurement order disagree", () => {
      const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
      observeClaude(
        { five_hour: { utilization: 0.4, resetsAt: NEXT_WEEK_RESET } },
        {
          observedAt: EARLIER,
        },
      );
      observeClaude(
        { five_hour: { utilization: 0.5, resetsAt: WEEK_RESET } },
        {
          observedAt: LATER,
        },
      );
      const line = String(
        warn.mock.calls.find((c) => String(c[0]).includes("moved backward"))?.[0],
      );
      expect(line).toContain("five_hour");
      // Both resets and both instants, because the line exists to describe a shape
      // nobody has seen — a bare "this happened" would not be actionable.
      expect(line).toContain(String(WEEK_RESET));
      expect(line).toContain(String(NEXT_WEEK_RESET));
      expect(line).toContain(LATER);
      warn.mockRestore();
    });

    it("stays silent for a stale snapshot, which is the ordinary case", () => {
      // Opening a project whose saved reading predates the window rolling over is
      // older on *both* axes. Warning here would fire on every project open and bury
      // the signal it exists for.
      const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
      observeClaude(
        { five_hour: { utilization: 0.4, resetsAt: NEXT_WEEK_RESET } },
        {
          observedAt: LATER,
        },
      );
      observeClaude(
        { five_hour: { utilization: 0.9, resetsAt: WEEK_RESET } },
        {
          observedAt: EARLIER,
        },
      );
      expect(warn).not.toHaveBeenCalled();
      warn.mockRestore();
    });

    it("stays silent when the held window carries no instant", () => {
      // `isNewer` ranks an absent instant last by convention, not by evidence, so
      // there is no measurement order for the reset to contradict.
      const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
      observeClaude({ five_hour: { utilization: 0.4, resetsAt: NEXT_WEEK_RESET } });
      observeClaude(
        { five_hour: { utilization: 0.5, resetsAt: WEEK_RESET } },
        {
          observedAt: LATER,
        },
      );
      expect(warn).not.toHaveBeenCalled();
      warn.mockRestore();
    });

    it("logs once for a condition that persists across readings", () => {
      // Every later reading carries the same earlier reset, so an unsuppressed line
      // would repeat until the stale reset elapsed — up to a week.
      const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
      observeClaude(
        { five_hour: { utilization: 0.4, resetsAt: NEXT_WEEK_RESET } },
        {
          observedAt: EARLIER,
        },
      );
      for (const at of ["2026-09-18T21:00:00Z", "2026-09-18T22:00:00Z", "2026-09-18T23:00:00Z"]) {
        observeClaude(
          { five_hour: { utilization: 0.5, resetsAt: WEEK_RESET } },
          { observedAt: at },
        );
      }
      expect(warn.mock.calls.filter((c) => String(c[0]).includes("moved backward"))).toHaveLength(
        1,
      );
      warn.mockRestore();
    });

    it("warns again after the key takes a window", () => {
      // The store never prunes, so a held window with a stale reset would otherwise
      // hold the mark for the life of the session and silence a second occurrence.
      const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
      observeClaude(
        { five_hour: { utilization: 0.4, resetsAt: NEXT_WEEK_RESET } },
        {
          observedAt: EARLIER,
        },
      );
      observeClaude(
        { five_hour: { utilization: 0.5, resetsAt: WEEK_RESET } },
        {
          observedAt: LATER,
        },
      );
      observeClaude(
        { five_hour: { utilization: 0.1, resetsAt: NEXT_WEEK_RESET + 1 } },
        {
          observedAt: "2026-09-18T22:00:00Z",
        },
      );
      observeClaude(
        { five_hour: { utilization: 0.6, resetsAt: WEEK_RESET } },
        {
          observedAt: "2026-09-18T23:00:00Z",
        },
      );
      expect(warn.mock.calls.filter((c) => String(c[0]).includes("moved backward"))).toHaveLength(
        2,
      );
      warn.mockRestore();
    });

    it("does not change which window is held", () => {
      // A diagnostic, not a policy. The reset still decides, so the meter behaves
      // exactly as it did before this line existed.
      const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
      observeClaude(
        { five_hour: { utilization: 0.4, resetsAt: NEXT_WEEK_RESET } },
        {
          observedAt: EARLIER,
        },
      );
      observeClaude(
        { five_hour: { utilization: 0.5, resetsAt: WEEK_RESET } },
        {
          observedAt: LATER,
        },
      );
      expect(heldWindows().five_hour?.window).toEqual({
        utilization: 0.4,
        resetsAt: NEXT_WEEK_RESET,
      });
      warn.mockRestore();
    });
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
              },
            },
          }
        : undefined,
    );
    await usage.loadPersistedUsage();
    // The stale `limit_reached` is dropped rather than restored. A file written
    // before the account read existed still carries the verdict that used to be
    // held beside the reading; loading it would paint a quota as spent from a
    // judgment nothing can retire, since nothing sets or clears the field now.
    expect(usage.harnessUsage.codex).toEqual({
      payload: WEEKLY,
      observed_at: "2026-09-18T19:00:00Z",
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

  it("round-trips a window map with each window's own context", async () => {
    // The whitelist copies `payload` wholesale and drops everything beside it, so
    // a field added outside it is silently stripped on every restart — invisible,
    // because the next turn repairs it. Asserting the values come back is what
    // separates working persistence from persistence that strips every entry.
    const stored = {
      claude_code: {
        payload: { status: "allowed", unifiedWindows: {} },
        observed_at: "2026-09-18T21:00:00Z",
        windows: {
          seven_day_overage_included: {
            window: { utilization: 1, resetsAt: WEEK_RESET },
            status: "rejected",
            rate_limit_type: "seven_day_overage_included",
            surpassed_threshold: 80,
            is_using_overage: false,
            observed_at: "2026-09-18T20:00:00Z",
            model: "claude-fable-5-1",
          },
        },
      },
    };
    invokeMock.mockImplementation(async (cmd) =>
      cmd === "get_harness_usage" ? { harnesses: stored } : undefined,
    );
    await usage.loadPersistedUsage();
    expect(usage.harnessUsage.claude_code?.windows).toEqual(stored.claude_code.windows);
  });

  it("does not restore permission to label a window", async () => {
    // The fill is a repair inside one live turn. A persisted turn id could only ever
    // authorize a stale one — naming a window, in a later session, from a turn that
    // had nothing to do with measuring it. Dropped on read, so a match is impossible
    // rather than merely unlikely.
    invokeMock.mockImplementation(async (cmd) =>
      cmd === "get_harness_usage"
        ? {
            harnesses: {
              claude_code: {
                payload: { status: "allowed", unifiedWindows: {} },
                observed_at: "2026-09-18T20:00:00Z",
                windows: {
                  seven_day_overage_included: {
                    window: { utilization: 1, resetsAt: WEEK_RESET },
                    observed_at: "2026-09-18T20:00:00Z",
                    turn_id: "turn-1",
                  },
                },
              },
            },
          }
        : undefined,
    );
    await usage.loadPersistedUsage();
    expect(heldWindows().seven_day_overage_included?.turn_id).toBeUndefined();
    usage.nameUsageModel("claude_code", "turn-1", "claude-opus-5");
    expect(heldWindows().seven_day_overage_included?.model).toBeUndefined();
  });

  it("recovers windows from an entry written before they were held individually", async () => {
    // The old shape carries its windows inside the payload and one instant for the
    // whole reading. The windows are lifted so they still render and still retire;
    // the reading-level instant is *not* copied onto them, because it would date
    // each window by when a different one was measured.
    invokeMock.mockImplementation(async (cmd) =>
      cmd === "get_harness_usage"
        ? {
            harnesses: {
              claude_code: {
                payload: {
                  status: "allowed",
                  unifiedWindows: {
                    seven_day_overage_included: { utilization: 1, resetsAt: WEEK_RESET },
                  },
                },
                observed_at: "2026-09-18T20:00:00Z",
                model: "claude-fable-5-1",
              },
            },
          }
        : undefined,
    );
    await usage.loadPersistedUsage();
    expect(heldWindows().seven_day_overage_included).toEqual({
      window: { utilization: 1, resetsAt: WEEK_RESET },
      status: "allowed",
      rate_limit_type: undefined,
      surpassed_threshold: undefined,
      is_using_overage: undefined,
      observed_at: undefined,
      model: "claude-fable-5-1",
      turn_id: undefined,
    });
  });

  it("leaves a Codex entry with no window map rather than inventing one", async () => {
    // The recovery is shape-driven rather than version-checked: a payload with no
    // window container yields none, which is what keeps one call serving every
    // harness.
    invokeMock.mockImplementation(async (cmd) =>
      cmd === "get_harness_usage"
        ? { harnesses: { codex: { payload: WEEKLY, observed_at: "2026-09-18T20:00:00Z" } } }
        : undefined,
    );
    await usage.loadPersistedUsage();
    expect(usage.harnessUsage.codex?.windows).toBeUndefined();
  });

  it.each([
    ["a non-object window map", "nope"],
    ["an array window map", []],
    ["a window entry that is not an object", { five_hour: "nope" }],
    ["a window entry with nothing stored", { five_hour: {} }],
    ["a non-string per-window instant", { five_hour: { window: {}, observed_at: 1_789_845_487 } }],
    ["a non-string per-window model", { five_hour: { window: {}, model: 5 } }],
  ])("drops the whole entry for %s", async (_case, windows) => {
    // One severity, deliberately. The file is machine-written, so a map that is not
    // the shape we write is not evidence of a window worth salvaging; repairing it
    // field by field would quietly demote a window to unlabelled or unranked.
    invokeMock.mockImplementation(async (cmd) =>
      cmd === "get_harness_usage"
        ? { harnesses: { claude_code: { payload: WEEKLY, windows } } }
        : undefined,
    );
    await usage.loadPersistedUsage();
    expect(usage.harnessUsage.claude_code).toBeUndefined();
  });

  it("survives a failed read with no readings rather than throwing", async () => {
    invokeMock.mockRejectedValue(new Error("unreadable"));
    await expect(usage.loadPersistedUsage()).resolves.toBeUndefined();
    expect(usage.harnessUsage.codex).toBeUndefined();
  });
});

describe("nameUsageModel", () => {
  const gated = { seven_day_overage_included: { utilization: 1, resetsAt: WEEK_RESET } };

  it("labels the windows this turn contributed once its init lands", () => {
    observeClaude(gated, { observedAt: "2026-09-18T20:00:00Z", turnId: "turn-1" });
    usage.nameUsageModel("claude_code", "turn-1", "claude-fable-5-1");
    expect(heldWindows().seven_day_overage_included?.model).toBe("claude-fable-5-1");
  });

  it("never relabels a window that already names a model", () => {
    // A later reading must not rewrite the label on a window an earlier model
    // delivered.
    observeClaude(gated, {
      observedAt: "2026-09-18T20:00:00Z",
      model: "claude-fable-5-1",
      turnId: "turn-1",
    });
    usage.nameUsageModel("claude_code", "turn-1", "claude-sonnet-5");
    expect(heldWindows().seven_day_overage_included?.model).toBe("claude-fable-5-1");
  });

  it("does not name a window a different turn contributed", () => {
    // Covers both races at once, because turn ids are unique across agents: the
    // second agent's `init` landing between the first agent's reading and its own,
    // and the same agent's *next* turn filling a blank left by a turn that died
    // before reporting its model. Either writes a model that never measured this
    // window, and under retention it renders until the window resets.
    observeClaude(gated, { observedAt: "2026-09-18T20:00:00Z", turnId: "turn-1" });
    usage.nameUsageModel("claude_code", "turn-2", "claude-opus-5");
    expect(heldWindows().seven_day_overage_included?.model).toBeUndefined();

    // Still repairable by the turn that actually measured it.
    usage.nameUsageModel("claude_code", "turn-1", "claude-fable-5-1");
    expect(heldWindows().seven_day_overage_included?.model).toBe("claude-fable-5-1");
  });

  it("does not match an unknown turn against an unknown contributor", () => {
    // Absent-equals-absent would make *every* unlabelled window eligible to any
    // fill, which is worse than the race being fixed. Both sides are checked.
    observeClaude(gated, { observedAt: "2026-09-18T20:00:00Z" });
    usage.nameUsageModel("claude_code", undefined, "claude-opus-5");
    expect(heldWindows().seven_day_overage_included?.model).toBeUndefined();
  });

  it("does not name a window whose contributing turn is unknown", () => {
    // Restored from disk: a later turn's model would be a guess about an older
    // measurement, so the window renders unlabelled instead.
    observeClaude(gated, { observedAt: "2026-09-18T20:00:00Z" });
    usage.nameUsageModel("claude_code", "turn-1", "claude-opus-5");
    expect(heldWindows().seven_day_overage_included?.model).toBeUndefined();
  });

  it.each([
    ["undefined", undefined],
    ["an empty string", ""],
  ])("ignores %s rather than storing it as a label", (_case, model) => {
    observeClaude(gated, { observedAt: "2026-09-18T20:00:00Z", turnId: "turn-1" });
    usage.nameUsageModel("claude_code", "turn-1", model);
    expect(heldWindows().seven_day_overage_included?.model).toBeUndefined();
  });

  it("does nothing when no reading is held", () => {
    usage.nameUsageModel("claude_code", "turn-1", "claude-fable-5-1");
    expect(usage.harnessUsage.claude_code).toBeUndefined();
  });

  it("does not write the file when there is no blank to fill", () => {
    observeClaude(gated, {
      observedAt: "2026-09-18T20:00:00Z",
      model: "claude-fable-5-1",
      turnId: "turn-1",
    });
    invokeMock.mockClear();
    usage.nameUsageModel("claude_code", "turn-1", "claude-sonnet-5");
    expect(invokeMock).not.toHaveBeenCalled();
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
    usage.observeUsage("codex", { payload: WEEKLY_LATER, observed_at: "2026-09-18T20:02:00Z" });
    usage.observeUsage("codex", { payload: ROLLED, observed_at: "2026-09-18T20:03:00Z" });
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
