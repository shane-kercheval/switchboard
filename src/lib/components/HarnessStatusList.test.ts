import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/svelte";
import HarnessStatusList from "./HarnessStatusList.svelte";
import type { HarnessKind } from "$lib/types";
import { ALL_HARNESSES, HARNESS_SETUP_URL } from "$lib/harnessDisplay";
import { _testing as availabilityTesting } from "$lib/harnessAvailability.svelte";

const invokeMock = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: Record<string, unknown>) => invokeMock(cmd, args),
}));
/// Every registered event handler (the store's and the component's), so a test
/// can model the backend's `harness_path_resolved` broadcast by firing them all.
const listenHandlers = vi.hoisted(() => [] as Array<() => void>);
vi.mock("@tauri-apps/api/event", () => ({
  listen: (_event: string, handler: () => void) => {
    listenHandlers.push(handler);
    return Promise.resolve(() => {});
  },
}));

type HarnessState = {
  installed: boolean;
  version: string | null;
  authed: boolean;
};

/// Which PATH the mocked backend claims its answers came from. Drives the
/// provisional ("capturing") and degraded ("fallback") renderings.
let pathSource: "capturing" | "login_shell" | "fallback";

const AUTH_CMD: Record<string, HarnessKind> = {
  check_claude_auth: "claude_code",
  check_codex_auth: "codex",
  check_gemini_auth: "gemini",
  check_antigravity_auth: "antigravity",
};

const ALL = ALL_HARNESSES;

let state: Record<HarnessKind, HarnessState>;

function setup(over?: Partial<Record<HarnessKind, Partial<HarnessState>>>): void {
  pathSource = "login_shell";
  state = {
    claude_code: { installed: true, version: "1.2.3", authed: true },
    codex: { installed: true, version: "0.9.0", authed: true },
    gemini: { installed: true, version: "2.0.0", authed: true },
    antigravity: { installed: true, version: "0.1.0", authed: true },
  };
  for (const h of ALL) {
    if (over?.[h]) state[h] = { ...state[h], ...over[h] };
  }
  invokeMock.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
    if (cmd === "get_harness_install_status") {
      const h = args?.harness as HarnessKind;
      return {
        installed: state[h].installed,
        version: state[h].version,
        path_source: pathSource,
      };
    }
    const authHarness = AUTH_CMD[cmd];
    if (authHarness !== undefined) {
      if (!state[authHarness].authed) throw new Error("not authenticated");
      return null;
    }
    if (cmd === "open_external_url") return null;
    // The Recheck button drops the backend's cached PATH; the re-probe that
    // follows reads whatever `state` says at that moment, which is how a test
    // models "the PATH capture succeeded this time."
    if (cmd === "recheck_harness_installs") return pathSource;
    throw new Error(`unexpected invoke: ${cmd}`);
  });
}

beforeEach(() => {
  invokeMock.mockReset();
  listenHandlers.length = 0;
  // The component reads install/version from the shared singleton store; reset
  // it so a prior test's probed values don't leak into this one's initial frame.
  availabilityTesting.reset();
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe("HarnessStatusList", () => {
  it("shows Installed + version and Authenticated for an installed, authed harness", async () => {
    setup();
    render(HarnessStatusList);
    await waitFor(() => {
      const install = screen.getByTestId("harness-install-claude_code");
      expect(install).toHaveTextContent("Installed");
      expect(install).toHaveTextContent("v1.2.3");
    });
    expect(screen.getByTestId("harness-auth-claude_code")).toHaveTextContent("Authenticated");
    // Installed harness shows no setup-guide button.
    expect(screen.queryByTestId("harness-setup-claude_code")).not.toBeInTheDocument();
  });

  it("renders all four harnesses uniformly — every row has an install column, no unsupported/? state", async () => {
    setup({ codex: { authed: false } });
    render(HarnessStatusList);
    await waitFor(() => {
      for (const h of ALL) {
        expect(screen.getByTestId(`harness-row-${h}`)).toBeInTheDocument();
        expect(screen.getByTestId(`harness-install-${h}`)).toBeInTheDocument();
      }
    });
    expect(screen.getByTestId("harness-status")).not.toHaveTextContent("?");
    expect(screen.getByTestId("harness-status")).not.toHaveTextContent("unsupported");
  });

  it("lists Antigravity above Gemini, and Gemini carries the availability note", async () => {
    // Display order deliberately deviates from ALL_HARNESSES here: Antigravity
    // superseded Gemini for individual Google accounts, so Gemini sits last
    // with a full-row note explaining who can still use it.
    setup();
    render(HarnessStatusList);
    await waitFor(() => {
      expect(screen.getByTestId("harness-row-gemini")).toBeInTheDocument();
    });
    const ids = screen.getAllByTestId(/^harness-row-/).map((el) => el.getAttribute("data-testid"));
    expect(ids.indexOf("harness-row-antigravity")).toBeLessThan(ids.indexOf("harness-row-gemini"));
    expect(ids).toHaveLength(ALL.length);

    const note = screen.getByTestId("harness-note-gemini");
    expect(note).toHaveTextContent(/no longer available on individual google accounts/i);
    expect(note).toHaveTextContent(/organization plan/i);
    // The note belongs to the Gemini row only.
    expect(screen.getByTestId("harness-row-gemini")).toContainElement(note);
  });

  it("separates the columns — a not-installed harness shows no auth status (auth is moot)", async () => {
    setup({ gemini: { installed: false, version: null, authed: false } });
    render(HarnessStatusList);
    await waitFor(() => {
      expect(screen.getByTestId("harness-install-gemini")).toHaveTextContent("Not installed");
    });
    // The auth column is present but carries no hint when the binary is missing.
    expect(screen.getByTestId("harness-auth-gemini")).toBeEmptyDOMElement();
  });

  it("not-installed harness offers a setup-guide button (next to the install status) that opens the docs via the opener", async () => {
    setup({ gemini: { installed: false, version: null } });
    render(HarnessStatusList);
    const button = await screen.findByTestId("harness-setup-gemini");
    expect(button).toHaveTextContent("Setup guide");

    await fireEvent.click(button);
    expect(invokeMock).toHaveBeenCalledWith("open_external_url", {
      url: HARNESS_SETUP_URL.gemini,
    });
  });

  it("installed-but-not-authed harness shows Installed and the authenticate hint in the auth column", async () => {
    setup({ codex: { authed: false } });
    render(HarnessStatusList);
    await waitFor(() => {
      expect(screen.getByTestId("harness-install-codex")).toHaveTextContent("v0.9.0");
    });
    expect(screen.getByTestId("harness-auth-codex")).toHaveTextContent(
      "run `codex login` to authenticate",
    );
    // It's installed, so no setup-guide button.
    expect(screen.queryByTestId("harness-setup-codex")).not.toBeInTheDocument();
  });

  it("re-probes install + auth when the window regains visibility", async () => {
    setup();
    render(HarnessStatusList);
    await waitFor(() =>
      expect(screen.getByTestId("harness-auth-claude_code")).toHaveTextContent("Authenticated"),
    );

    invokeMock.mockClear();
    fireEvent(document, new Event("visibilitychange"));

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("get_harness_install_status", {
        harness: "claude_code",
      });
    });
    expect(invokeMock).toHaveBeenCalledWith("check_codex_auth", undefined);
  });

  it("Refresh invalidates the cached PATH and recovers a wrongly not-installed harness", async () => {
    // The reported failure mode: a PATH capture that failed at launch makes
    // every harness read "not installed", and an ordinary refresh keeps saying
    // so because it re-probes the same cached PATH. The Refresh button must clear it.
    setup({
      claude_code: { installed: false, version: null },
      codex: { installed: false, version: null },
      gemini: { installed: false, version: null },
      antigravity: { installed: false, version: null },
    });
    render(HarnessStatusList);
    await waitFor(() =>
      expect(screen.getByTestId("harness-install-claude_code")).toHaveTextContent("Not installed"),
    );

    // The PATH resolves correctly on the next capture.
    for (const harness of ALL) state[harness] = { ...state[harness], installed: true };
    invokeMock.mockClear();

    await fireEvent.click(screen.getByTestId("harness-recheck"));

    await waitFor(() => {
      expect(screen.getByTestId("harness-install-claude_code")).toHaveTextContent("Installed");
    });
    expect(invokeMock).toHaveBeenCalledWith("recheck_harness_installs", undefined);
    // Auth is re-probed too — it runs after install resolves (the auth column
    // is meaningless until then), so a recheck that skipped it would leave the
    // row half-stale.
    await waitFor(() =>
      expect(screen.getByTestId("harness-auth-claude_code")).toHaveTextContent("Authenticated"),
    );
    expect(invokeMock).toHaveBeenCalledWith("check_claude_auth", undefined);
  });

  it("shows 'Checking…' rather than 'Not installed' while the PATH is still being read", async () => {
    // Detection can be answered from an interim PATH before the login shell
    // replies. Rendering that as a confident "Not installed" — with a Setup
    // guide telling the user to install software they already have — is the
    // symptom this whole mechanism exists to stop producing.
    setup({ claude_code: { installed: false, version: null } });
    pathSource = "capturing";
    render(HarnessStatusList);

    await waitFor(() =>
      expect(screen.getByTestId("harness-install-claude_code")).toHaveTextContent("Checking…"),
    );
    expect(screen.queryByTestId("harness-setup-claude_code")).not.toBeInTheDocument();
  });

  it("stays quiet about a degraded PATH when every CLI was found anyway", async () => {
    // The fallback did its job: nothing is missing, so there is nothing to act
    // on. Warning here trains the user to ignore the message on the day it
    // actually means something.
    setup();
    pathSource = "fallback";
    render(HarnessStatusList);

    await waitFor(() =>
      expect(screen.getByTestId("harness-install-claude_code")).toHaveTextContent("Installed"),
    );
    expect(screen.getByTestId("harness-path-note")).not.toHaveTextContent(/Couldn't read/i);
  });

  it("explains a degraded PATH when it actually cost us a CLI", async () => {
    // When the shell read fails *and* something reads as absent, "Not installed"
    // is a guess. Saying so is the difference between a diagnosable state and
    // the inexplicable one users report.
    setup({ claude_code: { installed: false, version: null } });
    pathSource = "fallback";
    render(HarnessStatusList);

    await waitFor(() =>
      expect(screen.getByTestId("harness-path-note")).toHaveTextContent(
        /couldn't read your shell's PATH/i,
      ),
    );
    // A fallback result is a real answer, not a provisional one, so the row
    // still reports the outcome rather than spinning forever.
    expect(screen.getByTestId("harness-install-claude_code")).toHaveTextContent("Not installed");
  });

  it("shows no note when the PATH read succeeded and nothing is wrong", async () => {
    // The note speaks only on problems. A healthy list with an explanatory
    // paragraph about login shells and PATHs is noise a normal user has no
    // context to parse.
    setup();
    render(HarnessStatusList);

    await waitFor(() =>
      expect(screen.getByTestId("harness-install-claude_code")).toHaveTextContent("Installed"),
    );
    expect(screen.getByTestId("harness-path-note").textContent?.trim()).toBe("");
  });

  it("says the check failed rather than claiming the CLI is absent", async () => {
    // A probe that errored is not an answer. Rendering it as "Not installed"
    // with a Setup guide sends the user to install what they may already have —
    // and the PATH note must not blame their shell for an unrelated failure.
    setup();
    invokeMock.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
      if (cmd === "get_harness_install_status") {
        if ((args?.harness as HarnessKind) === "claude_code") throw new Error("ipc died");
        return { installed: true, version: "1.0.0", path_source: "login_shell" };
      }
      return null;
    });
    render(HarnessStatusList);

    await waitFor(() =>
      expect(screen.getByTestId("harness-install-claude_code")).toHaveTextContent("Couldn't check"),
    );
    expect(screen.queryByTestId("harness-setup-claude_code")).not.toBeInTheDocument();
    expect(screen.getByTestId("harness-path-note")).not.toHaveTextContent(/Couldn't read/i);
  });

  it("tells the user when Refresh itself couldn't run, separately from a degraded PATH", async () => {
    setup();
    render(HarnessStatusList);
    await waitFor(() =>
      expect(screen.getByTestId("harness-install-claude_code")).toHaveTextContent("Installed"),
    );

    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "recheck_harness_installs") throw new Error("backend gone");
      if (cmd === "get_harness_install_status") {
        return { installed: true, version: "1.0.0", path_source: "login_shell" };
      }
      return null;
    });
    await fireEvent.click(screen.getByTestId("harness-recheck"));

    // Distinct from a degraded PATH: the re-read never ran, so blaming the
    // user's shell would point them at the wrong thing entirely.
    await waitFor(() =>
      expect(screen.getByTestId("harness-path-note")).toHaveTextContent(/Refresh couldn't run/i),
    );
    expect(screen.getByTestId("harness-path-note")).not.toHaveTextContent(/your shell's PATH/i);
  });

  it("clears the failed-recheck note once a later PATH read completes", async () => {
    // The note tells the user the backend was unreachable and to retry or
    // restart. A completed PATH read (the `harness_path_resolved` event) is
    // that recovery happening on its own — leaving the note up would pin an
    // alarming "restart the app" message above a healthy list for the rest of
    // the session on the strength of one transient failure.
    setup();
    render(HarnessStatusList);
    await waitFor(() =>
      expect(screen.getByTestId("harness-install-claude_code")).toHaveTextContent("Installed"),
    );

    const healthy = invokeMock.getMockImplementation();
    invokeMock.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
      if (cmd === "recheck_harness_installs") throw new Error("backend gone");
      return healthy?.(cmd, args);
    });
    await fireEvent.click(screen.getByTestId("harness-recheck"));
    await waitFor(() =>
      expect(screen.getByTestId("harness-path-note")).toHaveTextContent(/Refresh couldn't run/i),
    );

    for (const handler of listenHandlers) handler();

    await waitFor(() =>
      expect(screen.getByTestId("harness-path-note")).not.toHaveTextContent(
        /Refresh couldn't run/i,
      ),
    );
  });

  it("stays silent after a successful Refresh", async () => {
    // Success is the default and needs no confirmation - the rows themselves
    // flash the checking state and settle, which is the feedback.
    setup();
    render(HarnessStatusList);
    await waitFor(() =>
      expect(screen.getByTestId("harness-install-claude_code")).toHaveTextContent("Installed"),
    );

    await fireEvent.click(screen.getByTestId("harness-recheck"));

    await waitFor(() => expect(screen.getByTestId("harness-recheck")).toHaveTextContent("Refresh"));
    await waitFor(() =>
      expect(screen.getByTestId("harness-install-claude_code")).toHaveTextContent("Installed"),
    );
    expect(screen.getByTestId("harness-path-note").textContent?.trim()).toBe("");
  });

  it("re-enables Refresh as soon as the PATH re-read lands, not after the auth probes", async () => {
    // The button promises a PATH re-read. Holding it disabled through the auth
    // probes — a separate best-effort hint with its own column indicator — left
    // it greyed out for seconds after every row had visibly settled.
    setup();
    render(HarnessStatusList);
    await waitFor(() =>
      expect(screen.getByTestId("harness-install-claude_code")).toHaveTextContent("Installed"),
    );

    // Hold every auth probe open for the rest of the test.
    const authPending = new Promise<never>(() => {});
    invokeMock.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
      if (AUTH_CMD[cmd] !== undefined) return authPending;
      if (cmd === "recheck_harness_installs") return pathSource;
      if (cmd === "get_harness_install_status") {
        const h = args?.harness as HarnessKind;
        return {
          installed: state[h].installed,
          version: state[h].version,
          path_source: pathSource,
        };
      }
      return null;
    });

    const button = screen.getByTestId("harness-recheck");
    await fireEvent.click(button);

    // Auth is still outstanding; the button must not be waiting on it.
    await waitFor(() => expect(button).toBeEnabled());
    expect(screen.getByTestId("harness-auth-claude_code")).toHaveTextContent("Checking…");
  });

  it("disables Refresh while one is in flight so a slow re-capture can't be stacked", async () => {
    setup();
    render(HarnessStatusList);
    await waitFor(() =>
      expect(screen.getByTestId("harness-install-claude_code")).toHaveTextContent("Installed"),
    );

    // Hold the invalidation open to model the slow case this button exists for
    // (the re-capture re-runs the user's login shell).
    let releaseInvalidate!: () => void;
    invokeMock.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
      if (cmd === "recheck_harness_installs") {
        await new Promise<void>((resolve) => {
          releaseInvalidate = resolve;
        });
        return null;
      }
      if (cmd === "get_harness_install_status") {
        const h = args?.harness as HarnessKind;
        return {
          installed: state[h].installed,
          version: state[h].version,
          path_source: pathSource,
        };
      }
      return null;
    });

    const button = screen.getByTestId("harness-recheck");
    await fireEvent.click(button);
    await waitFor(() => expect(button).toBeDisabled());
    expect(button).toHaveTextContent("Refreshing…");

    releaseInvalidate();
    await waitFor(() => expect(button).toBeEnabled());
    expect(button).toHaveTextContent("Refresh");
  });
});
