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
