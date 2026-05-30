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

type HarnessState = { installed: boolean; version: string | null; authed: boolean };

const AUTH_CMD: Record<string, HarnessKind> = {
  check_claude_auth: "claude_code",
  check_codex_auth: "codex",
  check_gemini_auth: "gemini",
  check_antigravity_auth: "antigravity",
};

const ALL = ALL_HARNESSES;

let state: Record<HarnessKind, HarnessState>;

function setup(over?: Partial<Record<HarnessKind, Partial<HarnessState>>>): void {
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
      return { installed: state[h].installed, version: state[h].version };
    }
    const authHarness = AUTH_CMD[cmd];
    if (authHarness !== undefined) {
      if (!state[authHarness].authed) throw new Error("not authenticated");
      return null;
    }
    if (cmd === "open_external_url") return null;
    throw new Error(`unexpected invoke: ${cmd}`);
  });
}

beforeEach(() => {
  invokeMock.mockReset();
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
});
