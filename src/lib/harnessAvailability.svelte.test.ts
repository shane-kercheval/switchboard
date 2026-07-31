import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { waitFor } from "@testing-library/svelte";
import type { HarnessInstallStatus, HarnessKind } from "./types";
import { ALL_HARNESSES } from "./harnessDisplay";

const invokeMock = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: Record<string, unknown>) => invokeMock(cmd, args),
}));

const listenMock = vi.fn((_event: string, _handler: () => void) => Promise.resolve(() => {}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: (event: string, handler: () => void) => listenMock(event, handler),
}));

import {
  _testing,
  harnessAvailability,
  isPathDegraded,
  recheckHarnessAvailability,
  refreshHarnessAvailability,
} from "./harnessAvailability.svelte";

const ALL = ALL_HARNESSES;

afterEach(() => {
  _testing.reset();
  invokeMock.mockReset();
  listenMock.mockReset();
  listenMock.mockImplementation(() => Promise.resolve(() => {}));
  invokeMock.mockResolvedValue({ installed: false, version: null, path_source: "login_shell" });
});

describe("harnessAvailability store", () => {
  it("issues no probe until listener registration has completed", async () => {
    // Registration, not invocation. `listen` is an async IPC round-trip, so
    // "we called it first" is not the guarantee — a capture publishing between
    // the probe and the subscription landing still loses its event. Holding the
    // mocked `listen` pending is the only way to see that window; an
    // immediately-resolving mock collapses it and the test proves nothing.
    let registerListener!: () => void;
    listenMock.mockImplementation(
      () =>
        new Promise((resolve) => {
          registerListener = () => resolve(() => {});
        }),
    );

    const refresh = refreshHarnessAvailability();
    await waitFor(() => expect(listenMock).toHaveBeenCalledTimes(1));
    expect(invokeMock).not.toHaveBeenCalled();

    registerListener();
    await refresh;
    expect(invokeMock).toHaveBeenCalledWith("get_harness_install_status", {
      harness: "claude_code",
    });
  });

  it("attaches the listener once, not per refresh", async () => {
    await refreshHarnessAvailability();
    await refreshHarnessAvailability();
    expect(listenMock).toHaveBeenCalledWith("harness_path_resolved", expect.any(Function));
    expect(listenMock).toHaveBeenCalledTimes(1);
  });

  it("keeps probing when listener registration fails outright", async () => {
    // A `listen` that rejects most likely means the IPC bridge is down; the
    // refresh must still run rather than hanging on a listener that will never
    // attach.
    listenMock.mockRejectedValueOnce(new Error("bridge down"));
    invokeMock.mockResolvedValue({ installed: true, version: "1.0.0", path_source: "login_shell" });

    await refreshHarnessAvailability();

    expect(harnessAvailability.installed()).toEqual(ALL);
  });

  it("retries listener registration on a later refresh after a failure", async () => {
    // Caching the failure would disable the completion event for the rest of the
    // session: every later capture would stay provisional until the 30s backstop,
    // even after the bridge recovered. The assumption that `listen` and `invoke`
    // fail together is a likelihood, not a contract.
    let handler: (() => void) | undefined;
    listenMock.mockRejectedValueOnce(new Error("bridge down"));
    listenMock.mockImplementation((_event: string, h: () => void) => {
      handler = h;
      return Promise.resolve(() => {});
    });
    invokeMock.mockResolvedValue({ installed: false, version: null, path_source: "login_shell" });

    await refreshHarnessAvailability();
    expect(handler).toBeUndefined();

    await refreshHarnessAvailability();

    expect(listenMock).toHaveBeenCalledTimes(2);
    expect(handler).toBeDefined();

    // And the retried listener is live, not merely registered.
    invokeMock.mockResolvedValue({ installed: true, version: "1.0.0", path_source: "login_shell" });
    handler?.();
    await waitFor(() =>
      expect(harnessAvailability.availability("claude_code").binary).toBe("available"),
    );
  });

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
      return Promise.resolve({
        installed: true,
        version: `v-${harness}`,
        path_source: "login_shell",
      });
    });
    await refreshHarnessAvailability();
    for (const harness of ALL) {
      expect(harnessAvailability.status(harness)).toEqual({
        installed: true,
        version: `v-${harness}`,
        path_source: "login_shell",
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
          ? { installed: false, version: null, path_source: "login_shell" }
          : { installed: true, version: "1.0.0", path_source: "login_shell" },
      );
    });
    await refreshHarnessAvailability();
    expect(harnessAvailability.availability("gemini").binary).toBe("missing");
    // Order is deterministic by `HARNESSES` construction, not coincidence —
    // auto-create relies on a stable iteration order.
    expect(harnessAvailability.installed()).toEqual(["claude_code", "codex", "antigravity"]);
  });

  it("records a rejected probe as couldn't-check, not as absent", async () => {
    invokeMock.mockImplementation((_cmd: string, args?: Record<string, unknown>) => {
      const harness = args?.harness as HarnessKind;
      return harness === "codex"
        ? Promise.reject(new Error("probe blew up"))
        : Promise.resolve({ installed: true, version: "1.0.0", path_source: "login_shell" });
    });
    await refreshHarnessAvailability();
    // A probe that couldn't run is not an answer about the CLI. It fails closed
    // for gating, but it must not claim the CLI is absent — and emphatically must
    // not claim anything about the PATH, which was read perfectly here.
    expect(harnessAvailability.status("codex")).toBeNull();
    // Carried through the shared view, not flattened to `missing` — every gating
    // surface must fail closed, but none may explain a failed check as absence.
    expect(harnessAvailability.availability("codex").binary).toBe("probe_failed");
    expect(harnessAvailability.installed()).not.toContain("codex");
    expect(isPathDegraded()).toBe(false);
  });

  it("drops in-flight probe writes once reset has run (no cross-test leak)", async () => {
    // Model an un-awaited startup probe (as App.svelte fires on mount) that
    // resolves only *after* the owning test has torn down via `reset()`. Without
    // the epoch guard the late write repollutes the freshly-cleared store, which
    // is what flaked App.test.ts on slower CI runners.
    let release!: (value: HarnessInstallStatus) => void;
    const pending = new Promise<HarnessInstallStatus>((resolve) => {
      release = resolve;
    });
    invokeMock.mockReturnValue(pending);

    const inFlight = refreshHarnessAvailability();
    _testing.reset();
    release({ installed: false, version: null, path_source: "login_shell" });
    await inFlight;

    for (const harness of ALL) {
      expect(harnessAvailability.status(harness)).toBeNull();
      expect(harnessAvailability.availability(harness).binary).toBe("checking");
    }
  });

  it("invalidates the backend PATH cache before re-probing on recheck", async () => {
    // Ordering is the whole point: probing before invalidation would re-probe
    // against the same stale PATH and report the same wrong answer, which is
    // the state the user is trying to escape.
    const calls: string[] = [];
    invokeMock.mockImplementation((cmd: string) => {
      calls.push(cmd);
      return Promise.resolve(
        cmd === "recheck_harness_installs"
          ? "login_shell"
          : { installed: true, version: "1.0.0", path_source: "login_shell" },
      );
    });

    await recheckHarnessAvailability();

    expect(calls[0]).toBe("recheck_harness_installs");
    expect(calls.filter((cmd) => cmd === "get_harness_install_status")).toHaveLength(ALL.length);
    expect(harnessAvailability.installed()).toEqual(ALL);
  });

  it("still re-probes when the invalidation call fails", async () => {
    // A recheck that can't invalidate should degrade to an ordinary refresh,
    // not abort — the user pressed a button and must see *some* fresh answer.
    invokeMock.mockImplementation((cmd: string) =>
      cmd === "recheck_harness_installs"
        ? Promise.reject(new Error("no such command"))
        : Promise.resolve({ installed: true, version: "1.0.0", path_source: "login_shell" }),
    );

    await recheckHarnessAvailability();

    expect(harnessAvailability.installed()).toEqual(ALL);
  });

  it("clears stale statuses synchronously, before waiting on the backend", async () => {
    invokeMock.mockResolvedValue({ installed: true, version: "1.0.0", path_source: "login_shell" });
    await refreshHarnessAvailability();
    expect(harnessAvailability.availability("claude_code").binary).toBe("available");

    // The re-read can be slow (it re-runs the login shell). The list must not
    // keep showing the previous answer — complete with Setup-guide buttons —
    // under a "Refreshing…" button for the whole wait, so the clear happens
    // before the first await rather than after it.
    const releaseProbes: Array<() => void> = [];
    let releaseRecheck!: () => void;
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "recheck_harness_installs") {
        return new Promise((resolve) => {
          releaseRecheck = () => resolve("login_shell");
        });
      }
      return new Promise((resolve) => {
        releaseProbes.push(() =>
          resolve({ installed: false, version: null, path_source: "login_shell" }),
        );
      });
    });

    const inFlight = recheckHarnessAvailability();
    // No flush: the clear must already be visible, while the backend call is
    // still pending.
    for (const harness of ALL) {
      expect(harnessAvailability.availability(harness).binary).toBe("checking");
    }

    releaseRecheck();
    await waitFor(() => expect(releaseProbes).toHaveLength(ALL.length));
    for (const release of releaseProbes) release();
    await inFlight;
    expect(harnessAvailability.availability("claude_code").binary).toBe("missing");
  });

  it("drops a refresh that started before a recheck, even if it resolves last", async () => {
    // The store used to allow overlapping refreshes because probes were
    // idempotent. A recheck deliberately changes what a probe returns, so an
    // older refresh resolving last would revert a successful recheck to the
    // pre-recheck answer — the exact state the user pressed the button to escape.
    // One resolver per harness — the refresh probes all four concurrently.
    const releaseStale: Array<() => void> = [];
    invokeMock.mockImplementation(
      () =>
        new Promise((resolve) => {
          releaseStale.push(() =>
            resolve({ installed: false, version: null, path_source: "fallback" }),
          );
        }),
    );
    const staleRefresh = refreshHarnessAvailability();
    await waitFor(() => expect(releaseStale).toHaveLength(ALL.length));

    invokeMock.mockImplementation((cmd: string) =>
      Promise.resolve(
        cmd === "recheck_harness_installs"
          ? "login_shell"
          : { installed: true, version: "9.9.9", path_source: "login_shell" },
      ),
    );
    await recheckHarnessAvailability();
    expect(harnessAvailability.availability("claude_code").binary).toBe("available");

    for (const release of releaseStale) release();
    await staleRefresh;

    expect(harnessAvailability.availability("claude_code").binary).toBe("available");
    expect(harnessAvailability.status("claude_code")?.version).toBe("9.9.9");
  });

  it("re-runs a refresh superseded mid-flight by the PATH-resolved event", async () => {
    // The backend settling its PATH produces two unsynchronized messages: the
    // invoke reply (recheck / await_harness_path) and the harness_path_resolved
    // event. When the reply is processed first, the event handler's generation
    // bump lands in the middle of the refresh the caller is awaiting and drops
    // every one of its writes — auto-create then reads installed() from stale
    // slots and seeds too few agents, silently. An awaited refresh must retry
    // until its writes land. The handler's own replacement refresh is held
    // pending here so only the awaited refresh's retry can populate the store —
    // otherwise the replacement would mask a missing retry and prove nothing.
    let listener: (() => void) | undefined;
    listenMock.mockImplementation((_event: string, handler: () => void) => {
      listener = handler;
      return Promise.resolve(() => {});
    });

    const releaseStale: Array<() => void> = [];
    let call = 0;
    invokeMock.mockImplementation(() => {
      call += 1;
      // Calls 1-4: the awaited refresh's first attempt, held so the event can
      // land mid-flight. Calls 5-8: the event handler's replacement refresh,
      // held forever. Calls 9+: the awaited refresh's retry, resolving.
      if (call <= 4) {
        return new Promise((resolve) => {
          releaseStale.push(() =>
            resolve({ installed: false, version: null, path_source: "capturing" }),
          );
        });
      }
      if (call <= 8) return new Promise(() => {});
      return Promise.resolve({ installed: true, version: "9.9.9", path_source: "login_shell" });
    });

    const awaited = refreshHarnessAvailability();
    await waitFor(() => expect(releaseStale).toHaveLength(ALL.length));
    await waitFor(() => expect(listener).toBeDefined());

    listener?.();
    await waitFor(() => expect(invokeMock).toHaveBeenCalledTimes(8));

    for (const release of releaseStale) release();
    await awaited;

    expect(harnessAvailability.installed()).toEqual(ALL);
    expect(harnessAvailability.status("claude_code")?.version).toBe("9.9.9");
  });

  it("treats a negative result taken mid-capture as 'checking', not 'not installed'", async () => {
    // Nothing waits for the login-shell PATH, so an early probe can be answered
    // from an interim PATH. Reporting that as a confident "Not installed" is the
    // symptom this whole mechanism exists to stop producing.
    invokeMock.mockResolvedValue({ installed: false, version: null, path_source: "capturing" });
    await refreshHarnessAvailability();
    expect(harnessAvailability.availability("claude_code").binary).toBe("checking");
    expect(harnessAvailability.installed()).toEqual([]);
    expect(isPathDegraded()).toBe(false);

    // A *positive* mid-capture result is trusted: finding the CLI on the interim
    // PATH already proves it exists.
    invokeMock.mockResolvedValue({ installed: true, version: "1.0.0", path_source: "capturing" });
    await refreshHarnessAvailability();
    expect(harnessAvailability.availability("claude_code").binary).toBe("available");
  });

  it("bumps the generation on a PATH-resolved refresh so older probes can't win", async () => {
    // A publish is exactly the moment previously-issued probes stop being
    // authoritative. Without the bump, a probe issued against the interim PATH
    // can resolve last and overwrite the correction the event triggered.
    let listener: (() => void) | undefined;
    listenMock.mockImplementation((_event: string, handler: () => void) => {
      listener = handler;
      return Promise.resolve(() => {});
    });

    const releaseStale: Array<() => void> = [];
    invokeMock.mockImplementation(
      () =>
        new Promise((resolve) => {
          releaseStale.push(() =>
            resolve({ installed: false, version: null, path_source: "capturing" }),
          );
        }),
    );
    const staleRefresh = refreshHarnessAvailability();
    await waitFor(() => expect(releaseStale).toHaveLength(ALL.length));

    // The store attaches the listener itself, on the first refresh.
    await waitFor(() => expect(listener).toBeDefined());
    invokeMock.mockResolvedValue({ installed: true, version: "1.0.0", path_source: "login_shell" });
    listener?.();
    await waitFor(() =>
      expect(harnessAvailability.availability("claude_code").binary).toBe("available"),
    );

    for (const release of releaseStale) release();
    await staleRefresh;

    expect(harnessAvailability.availability("claude_code").binary).toBe("available");
  });

  describe("provisional backstop", () => {
    // The recovery path for a completion event that never arrives — a listener
    // that failed to attach, or a publish that beat registration. Without it a
    // lost event parks every row on "Checking…" indefinitely, which is a worse
    // failure than the wrong answer the event was added to prevent.
    beforeEach(() => {
      vi.useFakeTimers();
    });
    afterEach(() => {
      vi.useRealTimers();
    });

    it("re-probes when results are still provisional after the backstop window", async () => {
      invokeMock.mockResolvedValue({ installed: false, version: null, path_source: "capturing" });
      await refreshHarnessAvailability();
      const afterFirst = invokeMock.mock.calls.length;

      await vi.advanceTimersByTimeAsync(30_000);

      expect(invokeMock.mock.calls.length).toBeGreaterThan(afterFirst);
    });

    it("does not re-probe when the PATH already settled", async () => {
      invokeMock.mockResolvedValue({
        installed: true,
        version: "1.0.0",
        path_source: "login_shell",
      });
      await refreshHarnessAvailability();
      const afterFirst = invokeMock.mock.calls.length;

      await vi.advanceTimersByTimeAsync(30_000);

      // A settled answer needs no backstop; arming one would re-probe every
      // harness on a timer forever.
      expect(invokeMock.mock.calls.length).toBe(afterFirst);
    });

    it("cancels the pending backstop once a later refresh settles", async () => {
      invokeMock.mockResolvedValue({ installed: false, version: null, path_source: "capturing" });
      await refreshHarnessAvailability();

      // The completion event lands (modelled as the next refresh returning a
      // settled answer) before the window elapses.
      invokeMock.mockResolvedValue({
        installed: true,
        version: "1.0.0",
        path_source: "login_shell",
      });
      await refreshHarnessAvailability();
      const afterSettled = invokeMock.mock.calls.length;

      await vi.advanceTimersByTimeAsync(30_000);

      expect(invokeMock.mock.calls.length).toBe(afterSettled);
    });
  });

  it("reports a degraded PATH so the UI can explain a possibly-wrong answer", async () => {
    invokeMock.mockResolvedValue({ installed: false, version: null, path_source: "fallback" });
    await refreshHarnessAvailability();
    // A fallback result is real — not provisional — so it renders as missing.
    expect(harnessAvailability.availability("claude_code").binary).toBe("missing");
    expect(isPathDegraded()).toBe(true);
  });

  it("updates a previously-cached value on a later refresh", async () => {
    invokeMock.mockResolvedValue({ installed: false, version: null, path_source: "login_shell" });
    await refreshHarnessAvailability();
    expect(harnessAvailability.availability("claude_code").binary).toBe("missing");

    invokeMock.mockResolvedValue({ installed: true, version: "2.0.0", path_source: "login_shell" });
    await refreshHarnessAvailability();
    expect(harnessAvailability.availability("claude_code").binary).toBe("available");
    expect(harnessAvailability.status("claude_code")).toEqual({
      installed: true,
      version: "2.0.0",
      path_source: "login_shell",
    });
  });
});
