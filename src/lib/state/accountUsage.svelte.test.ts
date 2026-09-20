import { beforeEach, describe, expect, it, vi } from "vitest";

const invokeMock = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (name: string, args: unknown) => invokeMock(name, args),
}));

const accountUsage = await import("./accountUsage.svelte");
const usage = await import("./harnessUsage.svelte");

/// Coalescing and store handoff for the self-refreshing account read.
///
/// The read spawns a subprocess, so "how many run" is the property worth
/// pinning; what the payload *means* is `usageWindows.test.ts`'s subject.
beforeEach(() => {
  invokeMock.mockReset();
  invokeMock.mockResolvedValue(undefined);
  accountUsage._testing.reset();
  usage._testing.reset();
});

/// A read the test controls the completion of, so "while one is in flight" is a
/// real state rather than a race against the microtask queue.
function heldRead(): { release: () => void; calls: () => number } {
  let release = (): void => {};
  const gate = new Promise<void>((resolve) => {
    release = () => resolve();
  });
  let calls = 0;
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd !== "read_codex_account_usage") return undefined;
    calls += 1;
    await gate;
    return undefined;
  });
  return { release, calls: () => calls };
}

describe("coalescing", () => {
  it("collapses a burst into one in-flight read and exactly one follow-up", async () => {
    // The in-flight case is the *normal* path, not a burst edge: the read takes
    // seconds and fires at every turn end, so several agents finishing together
    // must not each spawn a subprocess.
    const read = heldRead();
    accountUsage.requestAccountUsageRefresh();
    await vi.waitFor(() => expect(read.calls()).toBe(1));

    accountUsage.requestAccountUsageRefresh();
    accountUsage.requestAccountUsageRefresh();
    accountUsage.requestAccountUsageRefresh();
    expect(read.calls()).toBe(1);

    read.release();
    await accountUsage._testing.settled();
    // One further read, not three: the requests that arrived mid-flight
    // collapsed into a single follow-up.
    expect(read.calls()).toBe(2);
  });

  it("runs a further read rather than answering the later request from the one in flight", async () => {
    // **Drain semantics, not dedupe**, and the distinction is the whole point.
    // A read that started before a turn ended returns a number predating that
    // turn's consumption. Serving the later request from it would systematically
    // understate usage at exactly the moment it changed.
    const read = heldRead();
    accountUsage.requestAccountUsageRefresh();
    await vi.waitFor(() => expect(read.calls()).toBe(1));
    accountUsage.requestAccountUsageRefresh();
    read.release();
    await accountUsage._testing.settled();
    expect(read.calls()).toBe(2);
  });

  it("starts a fresh read once the previous one has settled", async () => {
    // The in-flight marker must clear, or every later refresh is silently
    // dropped for the life of the session.
    accountUsage.requestAccountUsageRefresh();
    await accountUsage._testing.settled();
    accountUsage.requestAccountUsageRefresh();
    await accountUsage._testing.settled();
    expect(invokeMock.mock.calls.filter((c) => c[0] === "read_codex_account_usage")).toHaveLength(
      2,
    );
  });

  it("keeps working after a failed read rather than wedging", async () => {
    invokeMock.mockRejectedValueOnce(new Error("ipc down"));
    accountUsage.requestAccountUsageRefresh();
    await accountUsage._testing.settled();

    invokeMock.mockResolvedValue({ ordinaryUsageAllowed: true, rateLimitsByLimitId: {} });
    accountUsage.requestAccountUsageRefresh();
    await accountUsage._testing.settled();
    expect(usage.harnessUsage.codex?.payload).toEqual({
      ordinaryUsageAllowed: true,
      rateLimitsByLimitId: {},
    });
  });
});

describe("surviving a failure while handling a response", () => {
  /// A response that is delivered successfully and then throws while being
  /// handled — the case the `invoke` try/catch does not cover, and the only way
  /// the drain loop can be left holding its slot.
  function throwsWhenRead(): unknown {
    return {
      get rateLimitsByLimitId(): never {
        throw new Error("boom");
      },
    };
  }

  it("releases the slot after a throw rather than freezing the meter for the session", async () => {
    // Held, the slot stops every later read for the life of the process — the
    // same permanent freeze the backend's timeout exists to prevent, reached
    // far more cheaply. The next request must still get through.
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    invokeMock.mockResolvedValueOnce(throwsWhenRead());
    accountUsage.requestAccountUsageRefresh();
    await accountUsage._testing.settled();

    invokeMock.mockResolvedValue({ ordinaryUsageAllowed: true, rateLimitsByLimitId: {} });
    accountUsage.requestAccountUsageRefresh();
    await accountUsage._testing.settled();
    expect(usage.harnessUsage.codex?.payload).toEqual({
      ordinaryUsageAllowed: true,
      rateLimitsByLimitId: {},
    });
    warn.mockRestore();
  });

  it("still runs a request that arrived during a failed read", async () => {
    // The follow-up flag is set while the failing read is in flight. Catching
    // around the whole loop instead of per pass would drop that request
    // silently, losing a refresh the user's turn asked for.
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    let calls = 0;
    let release = (): void => {};
    const gate = new Promise<void>((resolve) => {
      release = () => resolve();
    });
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd !== "read_codex_account_usage") return undefined;
      calls += 1;
      if (calls === 1) {
        await gate;
        return throwsWhenRead();
      }
      return { ordinaryUsageAllowed: true, rateLimitsByLimitId: {} };
    });

    accountUsage.requestAccountUsageRefresh();
    await vi.waitFor(() => expect(calls).toBe(1));
    accountUsage.requestAccountUsageRefresh();
    release();
    await accountUsage._testing.settled();

    expect(calls).toBe(2);
    expect(usage.harnessUsage.codex?.payload).toEqual({
      ordinaryUsageAllowed: true,
      rateLimitsByLimitId: {},
    });
    warn.mockRestore();
  });
});

describe("reporting a response we could not read", () => {
  const unreadable = {
    ordinaryUsageAllowed: true,
    // No `normalModelSlug`: a valid payload from a server that stopped
    // emitting nulls, and the one condition that empties the panel silently.
    rateLimitsByLimitId: { codex: { limitId: "codex", primary: { usedPercent: 42 } } },
  };

  it("warns once for a condition that persists across reads", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    invokeMock.mockResolvedValue(unreadable);
    for (let i = 0; i < 3; i += 1) {
      accountUsage.requestAccountUsageRefresh();
      await accountUsage._testing.settled();
    }
    expect(
      warn.mock.calls.filter((c) => String(c[0]).includes("unreadable response")),
    ).toHaveLength(1);
    warn.mockRestore();
  });

  it("warns again after an intervening readable response", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    invokeMock.mockResolvedValue(unreadable);
    accountUsage.requestAccountUsageRefresh();
    await accountUsage._testing.settled();

    invokeMock.mockResolvedValue({ ordinaryUsageAllowed: true, rateLimitsByLimitId: {} });
    accountUsage.requestAccountUsageRefresh();
    await accountUsage._testing.settled();

    invokeMock.mockResolvedValue(unreadable);
    accountUsage.requestAccountUsageRefresh();
    await accountUsage._testing.settled();
    expect(
      warn.mock.calls.filter((c) => String(c[0]).includes("unreadable response")),
    ).toHaveLength(2);
    warn.mockRestore();
  });

  it("names the window that could not be read, not just the quota", async () => {
    // A quota has two windows; "something in `codex` was unreadable" does not
    // tell a reader which one, and two unreadable windows would otherwise log
    // the identical entry twice.
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    invokeMock.mockResolvedValue({
      rateLimitsByLimitId: {
        codex: {
          normalModelSlug: null,
          primary: { usedPercent: "nope" },
          secondary: { usedPercent: "nope" },
        },
      },
    });
    accountUsage.requestAccountUsageRefresh();
    await accountUsage._testing.settled();
    const line = String(warn.mock.calls.find((c) => String(c[0]).includes("unreadable"))?.[0]);
    expect(line).toContain("codex.primary");
    expect(line).toContain("codex.secondary");
    warn.mockRestore();
  });

  it("says so when a standing condition clears", async () => {
    // Otherwise the log shows a problem that appears never to have ended. The
    // backend's failure log reports its recovery for the same reason.
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const info = vi.spyOn(console, "info").mockImplementation(() => {});
    invokeMock.mockResolvedValue(unreadable);
    accountUsage.requestAccountUsageRefresh();
    await accountUsage._testing.settled();
    expect(info).not.toHaveBeenCalled();

    invokeMock.mockResolvedValue({ ordinaryUsageAllowed: true, rateLimitsByLimitId: {} });
    accountUsage.requestAccountUsageRefresh();
    await accountUsage._testing.settled();
    expect(info.mock.calls.filter((c) => String(c[0]).includes("readable again"))).toHaveLength(1);
    warn.mockRestore();
    info.mockRestore();
  });

  it("says nothing about an account whose only quota is a model reserve", async () => {
    // Legitimately empty, not drift. Warning here would fire on every refresh
    // for a user whose setup is perfectly normal.
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    invokeMock.mockResolvedValue({
      ordinaryUsageAllowed: true,
      rateLimitsByLimitId: {
        base_model_inference: { normalModelSlug: "gpt-5.6-luna", primary: { usedPercent: 5 } },
      },
    });
    accountUsage.requestAccountUsageRefresh();
    await accountUsage._testing.settled();
    expect(warn.mock.calls.filter((c) => String(c[0]).includes("unreadable response"))).toEqual([]);
    warn.mockRestore();
  });
});

describe("handing the reading to the store", () => {
  it("stores the response under its own wrapping, not a flattened bucket map", async () => {
    // The wrapping is what carries `ordinaryUsageAllowed` through the persisted
    // file's whitelist. Flattened, it would be written as a sibling top-level key
    // and silently dropped on load, leaving the account gate absent after every
    // restart.
    const payload = {
      ordinaryUsageAllowed: false,
      rateLimitsByLimitId: { codex: { limitId: "codex", normalModelSlug: null } },
    };
    invokeMock.mockResolvedValue(payload);
    accountUsage.requestAccountUsageRefresh();
    await accountUsage._testing.settled();
    expect(usage.harnessUsage.codex?.payload).toEqual(payload);
  });

  it("stamps the reading so it outranks anything restored from disk", async () => {
    invokeMock.mockResolvedValue({ ordinaryUsageAllowed: true, rateLimitsByLimitId: {} });
    accountUsage.requestAccountUsageRefresh();
    await accountUsage._testing.settled();
    expect(usage.harnessUsage.codex?.observed_at).toBeDefined();
  });

  it("leaves the held reading in place when there is no reading to be had", async () => {
    // `None` means "no newer number", never an error to surface. Every failure
    // mode collapses to it, so the meter keeps showing what it has rather than
    // blanking over a refresh nobody asked for.
    const held = { ordinaryUsageAllowed: true, rateLimitsByLimitId: { codex: {} } };
    usage.observeUsage("codex", { payload: held, observed_at: new Date().toISOString() });
    invokeMock.mockResolvedValue(null);
    accountUsage.requestAccountUsageRefresh();
    await accountUsage._testing.settled();
    expect(usage.harnessUsage.codex?.payload).toEqual(held);
  });
});
