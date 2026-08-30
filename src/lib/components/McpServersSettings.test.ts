import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/svelte";
import McpServersSettings from "./McpServersSettings.svelte";
import type { McpProviderInfo } from "$lib/types";

const invokeMock = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: Record<string, unknown>) => invokeMock(cmd, args),
}));

// Capture event listeners so a test can fire the backend's `prompts:synced`
// signal and assert the component re-refreshes.
const eventListeners = new Map<string, (e: { payload: unknown }) => void>();
vi.mock("@tauri-apps/api/event", () => ({
  listen: (event: string, handler: (e: { payload: unknown }) => void) => {
    eventListeners.set(event, handler);
    return Promise.resolve(() => eventListeners.delete(event));
  },
}));

// A mutable fake backend so add/remove reflect in the next list fetch.
let providers: McpProviderInfo[];
// Overridable per test: what sign_in does (default: resolve and mark signed
// in) and what the saved-provider probe returns.
let signInImpl: (name: string) => Promise<unknown>;
let removeImpl: (name: string) => Promise<unknown>;
let savedProbeResult: unknown;

// Mirrors the real backend: sign-in lands tokens immediately, but `status`
// stays whatever the last completed sync recorded until `prompts:synced`.
function markSignedIn(name: string): void {
  providers = providers.map((p) => (p.name === name ? { ...p, has_token: true } : p));
}

/// An OAuth provider row fixture.
function oauthProvider(overrides: Partial<McpProviderInfo> = {}): McpProviderInfo {
  return {
    name: "tiddly",
    url: "https://x/mcp",
    has_token: false,
    auth: { type: "oauth" },
    status: { state: "needs_auth" },
    ...overrides,
  };
}

async function chooseBearerAuth(): Promise<void> {
  await fireEvent.click(screen.getByTestId("mcp-auth-mode-option-bearer"));
}

beforeEach(() => {
  providers = [];
  signInImpl = async (name: string) => {
    markSignedIn(name);
    return null;
  };
  removeImpl = async (name: string) => {
    providers = providers.filter((p) => p.name !== name);
    return null;
  };
  savedProbeResult = { state: "ok", prompt_count: 4 };
  eventListeners.clear();
  invokeMock.mockReset();
  invokeMock.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
    switch (cmd) {
      case "list_mcp_providers":
        return providers;
      case "add_mcp_provider":
        providers = [
          ...providers,
          {
            name: args?.name as string,
            url: args?.url as string,
            has_token: args?.bearer !== null && args?.bearer !== undefined,
            auth: (args?.auth ?? { type: "bearer" }) as McpProviderInfo["auth"],
            status: { state: "unknown" },
          },
        ];
        return null;
      case "remove_mcp_provider":
        return removeImpl(args?.name as string);
      case "test_mcp_connection":
        return 3;
      case "sign_in_mcp_provider":
        return signInImpl(args?.name as string);
      case "sign_out_mcp_provider":
        providers = providers.map((p) =>
          p.name === (args?.name as string)
            ? { ...p, has_token: false, status: { state: "needs_auth" } }
            : p,
        );
        return null;
      case "test_saved_mcp_provider":
        return savedProbeResult;
      case "sync_prompts":
        return null;
      default:
        throw new Error(`unexpected invoke: ${cmd}`);
    }
  });
});

afterEach(() => {
  vi.useRealTimers();
});

describe("McpServersSettings", () => {
  it("lists configured providers with status on mount", async () => {
    providers = [
      {
        name: "team",
        url: "https://x/mcp",
        has_token: true,
        auth: { type: "bearer" },
        status: { state: "ok", prompt_count: 2 },
      },
    ];
    render(McpServersSettings);
    await waitFor(() => expect(screen.getByTestId("mcp-row-team")).toBeInTheDocument());
    expect(screen.getByTestId("mcp-status-team")).toHaveTextContent("2 prompts");
  });

  it("flags a missing token", async () => {
    providers = [
      {
        name: "team",
        url: "https://x",
        has_token: false,
        auth: { type: "bearer" },
        status: { state: "unknown" },
      },
    ];
    render(McpServersSettings);
    await waitFor(() => expect(screen.getByTestId("mcp-row-team")).toBeInTheDocument());
    expect(screen.getByTestId("mcp-row-team")).toHaveTextContent("no token");
  });

  it("disables sync prompts when no MCP servers are configured", async () => {
    render(McpServersSettings);
    await waitFor(() => expect(screen.getByTestId("mcp-empty")).toBeInTheDocument());

    const sync = screen.getByTestId("mcp-sync") as HTMLButtonElement;
    expect(sync.disabled).toBe(true);

    await fireEvent.click(sync);
    expect(invokeMock).not.toHaveBeenCalledWith("sync_prompts");
  });

  it("enables sync prompts when an MCP server is configured", async () => {
    providers = [
      {
        name: "team",
        url: "https://x",
        has_token: false,
        auth: { type: "bearer" },
        status: { state: "unknown" },
      },
    ];
    render(McpServersSettings);
    await waitFor(() => expect(screen.getByTestId("mcp-row-team")).toBeInTheDocument());

    const sync = screen.getByTestId("mcp-sync") as HTMLButtonElement;
    expect(sync.disabled).toBe(false);

    await fireEvent.click(sync);
    expect(invokeMock.mock.calls.some(([cmd]) => cmd === "sync_prompts")).toBe(true);
  });

  it("rejects the reserved name `local` and blocks submit", async () => {
    render(McpServersSettings);
    await waitFor(() => expect(screen.getByTestId("mcp-empty")).toBeInTheDocument());
    await fireEvent.input(screen.getByTestId("mcp-name"), { target: { value: "local" } });
    await fireEvent.input(screen.getByTestId("mcp-url"), { target: { value: "https://x" } });
    expect(screen.getByTestId("mcp-name-error")).toBeInTheDocument();
    expect((screen.getByTestId("mcp-add") as HTMLButtonElement).disabled).toBe(true);
  });

  it("adds a provider (bearer null when blank) and refreshes the list", async () => {
    render(McpServersSettings);
    await waitFor(() => expect(screen.getByTestId("mcp-empty")).toBeInTheDocument());
    await chooseBearerAuth();
    await fireEvent.input(screen.getByTestId("mcp-name"), { target: { value: "team" } });
    await fireEvent.input(screen.getByTestId("mcp-url"), { target: { value: "https://x/mcp" } });
    await fireEvent.click(screen.getByTestId("mcp-add"));
    await waitFor(() => expect(screen.getByTestId("mcp-row-team")).toBeInTheDocument());
    const addCall = invokeMock.mock.calls.find(([c]) => c === "add_mcp_provider");
    expect(addCall?.[1]).toMatchObject({ name: "team", url: "https://x/mcp", bearer: null });
  });

  it("sends the bearer when provided", async () => {
    render(McpServersSettings);
    await waitFor(() => expect(screen.getByTestId("mcp-empty")).toBeInTheDocument());
    await chooseBearerAuth();
    await fireEvent.input(screen.getByTestId("mcp-name"), { target: { value: "team" } });
    await fireEvent.input(screen.getByTestId("mcp-url"), { target: { value: "https://x" } });
    await fireEvent.input(screen.getByTestId("mcp-bearer"), { target: { value: "tok" } });
    await fireEvent.click(screen.getByTestId("mcp-add"));
    await waitFor(() => {
      const addCall = invokeMock.mock.calls.find(([c]) => c === "add_mcp_provider");
      expect(addCall?.[1]).toMatchObject({ name: "team", url: "https://x", bearer: "tok" });
    });
  });

  it("removes a provider", async () => {
    providers = [
      {
        name: "team",
        url: "https://x",
        has_token: false,
        auth: { type: "bearer" },
        status: { state: "unknown" },
      },
    ];
    render(McpServersSettings);
    await waitFor(() => expect(screen.getByTestId("mcp-row-team")).toBeInTheDocument());
    await fireEvent.click(screen.getByTestId("mcp-remove-team"));
    await waitFor(() => expect(screen.queryByTestId("mcp-row-team")).not.toBeInTheDocument());
    expect(invokeMock).toHaveBeenCalledWith("remove_mcp_provider", { name: "team" });
  });

  it("refreshes a just-added provider's status on the prompts:synced event", async () => {
    providers = [
      {
        name: "team",
        url: "https://x",
        has_token: false,
        auth: { type: "bearer" },
        status: { state: "unknown" },
      },
    ];
    render(McpServersSettings);
    await waitFor(() => expect(screen.getByTestId("mcp-status-team")).toBeInTheDocument());
    expect(screen.getByTestId("mcp-status-team")).not.toHaveTextContent("2 prompts");

    // Background sync completes: backend now reports a real status.
    providers = [
      {
        name: "team",
        url: "https://x",
        has_token: false,
        auth: { type: "bearer" },
        status: { state: "ok", prompt_count: 2 },
      },
    ];
    eventListeners.get("prompts:synced")?.({ payload: null });

    await waitFor(() =>
      expect(screen.getByTestId("mcp-status-team")).toHaveTextContent("2 prompts"),
    );
  });

  it("test connection reports the prompt count", async () => {
    render(McpServersSettings);
    await waitFor(() => expect(screen.getByTestId("mcp-empty")).toBeInTheDocument());
    await chooseBearerAuth();
    await fireEvent.input(screen.getByTestId("mcp-url"), { target: { value: "https://x/mcp" } });
    await fireEvent.click(screen.getByTestId("mcp-test"));
    await waitFor(() =>
      expect(screen.getByTestId("mcp-test-result")).toHaveTextContent("3 prompts"),
    );
  });
});

describe("McpServersSettings — OAuth", () => {
  it("defaults to OAuth first and adds with the oauth mode", async () => {
    render(McpServersSettings);
    await waitFor(() => expect(screen.getByTestId("mcp-empty")).toBeInTheDocument());

    const authOptions = screen.getAllByRole("radio");
    expect(authOptions.map((option) => option.textContent)).toEqual([
      "OAuth sign-in",
      "Bearer token",
    ]);
    expect(authOptions[0]).toHaveAttribute("aria-checked", "true");
    expect(screen.queryByTestId("mcp-bearer")).not.toBeInTheDocument();
    // The pre-save Test cannot authenticate an OAuth server: the button is
    // replaced by the add → sign in → test guidance.
    expect(screen.queryByTestId("mcp-test")).not.toBeInTheDocument();
    expect(screen.getByTestId("mcp-oauth-test-hint")).toBeInTheDocument();

    await fireEvent.input(screen.getByTestId("mcp-name"), { target: { value: "tiddly" } });
    await fireEvent.input(screen.getByTestId("mcp-url"), { target: { value: "https://x/mcp" } });
    await fireEvent.click(screen.getByTestId("mcp-add"));
    await waitFor(() => expect(screen.getByTestId("mcp-row-tiddly")).toBeInTheDocument());
    const addCall = invokeMock.mock.calls.find(([c]) => c === "add_mcp_provider");
    expect(addCall?.[1]).toMatchObject({
      name: "tiddly",
      url: "https://x/mcp",
      auth: { type: "oauth" },
      bearer: null,
    });
    expect(screen.getByTestId("mcp-auth-mode")).toHaveAttribute("data-value", "oauth");
  });

  it("renders needs_auth as its own status with the sign-in affordance", async () => {
    providers = [oauthProvider()];
    render(McpServersSettings);
    await waitFor(() => expect(screen.getByTestId("mcp-row-tiddly")).toBeInTheDocument());

    expect(screen.getByTestId("mcp-status-tiddly")).toHaveTextContent("Needs sign-in");
    expect(screen.getByTestId("mcp-row-tiddly")).toHaveTextContent("not signed in");
    const signIn = screen.getByTestId("mcp-sign-in-tiddly") as HTMLButtonElement;
    expect(signIn.disabled).toBe(false);
    expect(signIn.parentElement).not.toHaveAttribute("tabindex");
    // Signed out: no sign-out or row-level test yet.
    expect(screen.queryByTestId("mcp-sign-out-tiddly")).not.toBeInTheDocument();
    expect(screen.queryByTestId("mcp-test-tiddly")).not.toBeInTheDocument();
  });

  it("sign-in shows a pending state that survives the wait, then refreshes", async () => {
    providers = [oauthProvider()];
    let resolveSignIn: (() => void) | undefined;
    signInImpl = (name: string) =>
      new Promise<unknown>((resolve) => {
        resolveSignIn = () => {
          markSignedIn(name);
          resolve(null);
        };
      });
    render(McpServersSettings);
    await waitFor(() => expect(screen.getByTestId("mcp-sign-in-tiddly")).toBeInTheDocument());

    await fireEvent.click(screen.getByTestId("mcp-sign-in-tiddly"));
    // Pending while the user is in the browser; the row's actions are held.
    await waitFor(() =>
      expect(screen.getByTestId("mcp-sign-in-tiddly")).toHaveTextContent("Waiting for browser…"),
    );
    expect((screen.getByTestId("mcp-remove-tiddly") as HTMLButtonElement).disabled).toBe(true);

    resolveSignIn?.();
    // The status stays stale until the background sync lands; the success
    // notice is what confirms the outcome in the meantime.
    await waitFor(() =>
      expect(screen.getByTestId("mcp-notice-tiddly")).toHaveTextContent("Signed in."),
    );
    expect(screen.getByTestId("mcp-sign-out-tiddly")).toBeInTheDocument();
    expect(screen.getByTestId("mcp-sign-in-tiddly")).toHaveTextContent("Sign in");
    expect((screen.getByTestId("mcp-remove-tiddly") as HTMLButtonElement).disabled).toBe(false);
  });

  it("a rejected sign-in surfaces the backend's message and clears pending", async () => {
    providers = [oauthProvider()];
    signInImpl = async () => {
      throw new Error('OAuth flow failed for MCP provider "tiddly": consent denied');
    };
    render(McpServersSettings);
    await waitFor(() => expect(screen.getByTestId("mcp-sign-in-tiddly")).toBeInTheDocument());

    await fireEvent.click(screen.getByTestId("mcp-sign-in-tiddly"));
    await waitFor(() =>
      expect(screen.getByTestId("mcp-notice-tiddly")).toHaveTextContent("consent denied"),
    );
    // Pending clears after the finally-refresh completes, one tick later.
    await waitFor(() =>
      expect(screen.getByTestId("mcp-sign-in-tiddly")).toHaveTextContent("Sign in"),
    );
    expect((screen.getByTestId("mcp-sign-in-tiddly") as HTMLButtonElement).disabled).toBe(false);
  });

  it("re-sign-in on a signed-in row launches directly and a double-click is inert", async () => {
    // No confirmation panel (removed — see the plan's M4 supersession note:
    // point-of-use auto-sign-in made an abandoned re-sign-in self-healing).
    // Double-click safety comes from the pending state disabling the button.
    providers = [oauthProvider({ has_token: true, status: { state: "ok", prompt_count: 3 } })];
    let resolveSignIn: (() => void) | undefined;
    signInImpl = (name: string) =>
      new Promise<unknown>((resolve) => {
        resolveSignIn = () => {
          markSignedIn(name);
          resolve(null);
        };
      });
    render(McpServersSettings);
    await waitFor(() => expect(screen.getByTestId("mcp-sign-in-tiddly")).toBeInTheDocument());

    await fireEvent.click(screen.getByTestId("mcp-sign-in-tiddly"));
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("sign_in_mcp_provider", { name: "tiddly" }),
    );
    // The second click of a double-click lands on a disabled, pending button.
    expect((screen.getByTestId("mcp-sign-in-tiddly") as HTMLButtonElement).disabled).toBe(true);
    await fireEvent.click(screen.getByTestId("mcp-sign-in-tiddly"));
    expect(invokeMock.mock.calls.filter(([c]) => c === "sign_in_mcp_provider")).toHaveLength(1);

    resolveSignIn?.();
    await waitFor(() =>
      expect(screen.getByTestId("mcp-notice-tiddly")).toHaveTextContent("Signed in."),
    );
  });

  it("sign-out round-trips back to needs_auth", async () => {
    providers = [oauthProvider({ has_token: true, status: { state: "ok", prompt_count: 3 } })];
    render(McpServersSettings);
    await waitFor(() => expect(screen.getByTestId("mcp-sign-out-tiddly")).toBeInTheDocument());

    await fireEvent.click(screen.getByTestId("mcp-sign-out-tiddly"));
    await waitFor(() =>
      expect(screen.getByTestId("mcp-status-tiddly")).toHaveTextContent("Needs sign-in"),
    );
    expect(invokeMock).toHaveBeenCalledWith("sign_out_mcp_provider", { name: "tiddly" });
    // Signed out again: the row-level test disappears with the credentials.
    expect(screen.queryByTestId("mcp-test-tiddly")).not.toBeInTheDocument();
  });

  it("row-level Test renders success, needs_auth, and failure outcomes", async () => {
    providers = [oauthProvider({ has_token: true, status: { state: "ok", prompt_count: 3 } })];
    render(McpServersSettings);
    await waitFor(() => expect(screen.getByTestId("mcp-test-tiddly")).toBeInTheDocument());

    savedProbeResult = { state: "ok", prompt_count: 4 };
    await fireEvent.click(screen.getByTestId("mcp-test-tiddly"));
    await waitFor(() =>
      expect(screen.getByTestId("mcp-notice-tiddly")).toHaveTextContent("Connected — 4 prompts."),
    );

    savedProbeResult = { state: "needs_auth" };
    await fireEvent.click(screen.getByTestId("mcp-test-tiddly"));
    await waitFor(() =>
      expect(screen.getByTestId("mcp-notice-tiddly")).toHaveTextContent("Needs sign-in."),
    );

    savedProbeResult = { state: "errored", message: "connection refused" };
    await fireEvent.click(screen.getByTestId("mcp-test-tiddly"));
    await waitFor(() =>
      expect(screen.getByTestId("mcp-notice-tiddly")).toHaveTextContent("connection refused"),
    );
  });

  it("bearer rows keep the bearer-only affordances", async () => {
    providers = [
      {
        name: "team",
        url: "https://x",
        has_token: false,
        auth: { type: "bearer" },
        status: { state: "unknown" },
      },
    ];
    render(McpServersSettings);
    await waitFor(() => expect(screen.getByTestId("mcp-row-team")).toBeInTheDocument());

    // No OAuth actions on a bearer row; the hint stays token-flavored.
    expect(screen.queryByTestId("mcp-sign-in-team")).not.toBeInTheDocument();
    expect(screen.queryByTestId("mcp-sign-out-team")).not.toBeInTheDocument();
    expect(screen.queryByTestId("mcp-test-team")).not.toBeInTheDocument();
    expect(screen.getByTestId("mcp-row-team")).toHaveTextContent("no token");
  });
});

describe("McpServersSettings — OAuth delta hardening", () => {
  it("a store_unavailable row disables Sign in with the keychain reason", async () => {
    providers = [oauthProvider({ status: { state: "store_unavailable" } })];
    render(McpServersSettings);
    await waitFor(() => expect(screen.getByTestId("mcp-row-tiddly")).toBeInTheDocument());

    const signIn = screen.getByTestId("mcp-sign-in-tiddly") as HTMLButtonElement;
    const tooltipTrigger = signIn.parentElement!;
    expect(signIn.disabled).toBe(true);
    expect(signIn).not.toHaveAttribute("title");
    expect(tooltipTrigger).toHaveAttribute("tabindex", "0");
    vi.useFakeTimers({ shouldAdvanceTime: true });
    await fireEvent.focus(tooltipTrigger);
    await vi.advanceTimersByTimeAsync(1100);
    expect(screen.getByTestId("tooltip-content")).toHaveTextContent("keychain");
    // The row is still recognizably an OAuth row (button present, not hidden),
    // and Remove stays available.
    expect((screen.getByTestId("mcp-remove-tiddly") as HTMLButtonElement).disabled).toBe(false);
  });

  it("probe outcomes reuse the status tones, not a pass/fail flattening", async () => {
    providers = [oauthProvider({ has_token: true, status: { state: "ok", prompt_count: 3 } })];
    render(McpServersSettings);
    await waitFor(() => expect(screen.getByTestId("mcp-test-tiddly")).toBeInTheDocument());

    savedProbeResult = { state: "needs_auth" };
    await fireEvent.click(screen.getByTestId("mcp-test-tiddly"));
    await waitFor(() =>
      expect(screen.getByTestId("mcp-notice-tiddly")).toHaveTextContent("Needs sign-in."),
    );
    expect(screen.getByTestId("mcp-notice-tiddly")).toHaveClass("text-accent");

    savedProbeResult = { state: "store_unavailable" };
    await fireEvent.click(screen.getByTestId("mcp-test-tiddly"));
    await waitFor(() =>
      expect(screen.getByTestId("mcp-notice-tiddly")).toHaveTextContent("keychain"),
    );
    expect(screen.getByTestId("mcp-notice-tiddly")).toHaveClass("text-warning");
  });

  it("a failed sign-in refreshes the row and keeps its error notice", async () => {
    // A failed re-sign-in can already have signed the user out (the
    // pre-browser token wipe): the row must show the new reality and the
    // error together, not the old world until a background sync lands.
    providers = [oauthProvider({ has_token: true, status: { state: "ok", prompt_count: 3 } })];
    signInImpl = async (name: string) => {
      providers = providers.map((p) =>
        p.name === name ? { ...p, has_token: false, status: { state: "needs_auth" } } : p,
      );
      throw new Error("the browser sign-in was not completed");
    };
    render(McpServersSettings);
    await waitFor(() => expect(screen.getByTestId("mcp-sign-in-tiddly")).toBeInTheDocument());

    await fireEvent.click(screen.getByTestId("mcp-sign-in-tiddly"));
    await waitFor(() =>
      expect(screen.getByTestId("mcp-notice-tiddly")).toHaveTextContent("was not completed"),
    );
    // The refresh landed alongside the notice — the error survived it.
    expect(screen.getByTestId("mcp-status-tiddly")).toHaveTextContent("Needs sign-in");
    expect(screen.queryByTestId("mcp-sign-out-tiddly")).not.toBeInTheDocument();
  });

  it("the success notice clears once a background sync delivers the status", async () => {
    providers = [oauthProvider()];
    render(McpServersSettings);
    await waitFor(() => expect(screen.getByTestId("mcp-sign-in-tiddly")).toBeInTheDocument());

    await fireEvent.click(screen.getByTestId("mcp-sign-in-tiddly"));
    await waitFor(() =>
      expect(screen.getByTestId("mcp-notice-tiddly")).toHaveTextContent("Signed in."),
    );

    // The background sync completes: real status arrives, the transient
    // notice goes (the status now carries the same fact).
    providers = providers.map((p) => ({ ...p, status: { state: "ok", prompt_count: 2 } }));
    eventListeners.get("prompts:synced")?.({ payload: null });
    await waitFor(() =>
      expect(screen.getByTestId("mcp-status-tiddly")).toHaveTextContent("2 prompts"),
    );
    expect(screen.queryByTestId("mcp-notice-tiddly")).not.toBeInTheDocument();
  });

  it("a mid-flight prompts:synced leaves the pending sign-in untouched", async () => {
    // The ordering race AGENTS.md calls out: a background sync completing
    // while a row action is pending must refresh the row's data without
    // clearing or corrupting its pending state.
    providers = [oauthProvider()];
    let resolveSignIn: (() => void) | undefined;
    signInImpl = (name: string) =>
      new Promise<unknown>((resolve) => {
        resolveSignIn = () => {
          markSignedIn(name);
          resolve(null);
        };
      });
    render(McpServersSettings);
    await waitFor(() => expect(screen.getByTestId("mcp-sign-in-tiddly")).toBeInTheDocument());
    await fireEvent.click(screen.getByTestId("mcp-sign-in-tiddly"));
    await waitFor(() =>
      expect(screen.getByTestId("mcp-sign-in-tiddly")).toHaveTextContent("Waiting for browser…"),
    );

    // An unrelated sync completes mid-flight with fresh row data.
    providers = providers.map((p) => ({ ...p, status: { state: "errored", message: "boom" } }));
    eventListeners.get("prompts:synced")?.({ payload: null });
    await waitFor(() => expect(screen.getByTestId("mcp-status-tiddly")).toHaveTextContent("Error"));
    // The pending state survived the refresh.
    expect(screen.getByTestId("mcp-sign-in-tiddly")).toHaveTextContent("Waiting for browser…");
    expect((screen.getByTestId("mcp-remove-tiddly") as HTMLButtonElement).disabled).toBe(true);

    // And the flow still completes normally afterwards.
    resolveSignIn?.();
    await waitFor(() =>
      expect(screen.getByTestId("mcp-notice-tiddly")).toHaveTextContent("Signed in."),
    );
    expect(screen.getByTestId("mcp-sign-in-tiddly")).toHaveTextContent("Sign in");
  });

  it("a bearer test verdict disappears when the form switches to OAuth", async () => {
    render(McpServersSettings);
    await waitFor(() => expect(screen.getByTestId("mcp-empty")).toBeInTheDocument());
    await chooseBearerAuth();
    await fireEvent.input(screen.getByTestId("mcp-url"), { target: { value: "https://x/mcp" } });
    await fireEvent.click(screen.getByTestId("mcp-test"));
    await waitFor(() =>
      expect(screen.getByTestId("mcp-test-result")).toHaveTextContent("3 prompts"),
    );

    // Switching modes retires the verdict — it tested a bearer configuration.
    await fireEvent.click(screen.getByTestId("mcp-auth-mode-option-oauth"));
    expect(screen.queryByTestId("mcp-test-result")).not.toBeInTheDocument();
    // Switching back (inputs unchanged) restores it: the stamp still matches.
    await fireEvent.click(screen.getByTestId("mcp-auth-mode-option-bearer"));
    expect(screen.getByTestId("mcp-test-result")).toHaveTextContent("3 prompts");
    // Editing the URL retires it too.
    await fireEvent.input(screen.getByTestId("mcp-url"), { target: { value: "https://y/mcp" } });
    expect(screen.queryByTestId("mcp-test-result")).not.toBeInTheDocument();
  });

  it("an in-flight bearer test resolving after a mode switch renders nothing", async () => {
    let resolveTest: ((count: number) => void) | undefined;
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "list_mcp_providers") return providers;
      if (cmd === "test_mcp_connection")
        return new Promise<number>((resolve) => {
          resolveTest = resolve;
        });
      return null;
    });
    render(McpServersSettings);
    await waitFor(() => expect(screen.getByTestId("mcp-empty")).toBeInTheDocument());
    await chooseBearerAuth();
    await fireEvent.input(screen.getByTestId("mcp-url"), { target: { value: "https://x/mcp" } });
    await fireEvent.click(screen.getByTestId("mcp-test"));

    await fireEvent.click(screen.getByTestId("mcp-auth-mode-option-oauth"));
    resolveTest?.(3);
    // Absence assertion after a microtask flush.
    await Promise.resolve();
    expect(screen.queryByTestId("mcp-test-result")).not.toBeInTheDocument();
  });

  it("a failed removal reports on the row it concerns", async () => {
    providers = [oauthProvider()];
    removeImpl = async () => {
      throw new Error("a sign-in or sign-out for this provider is already in progress");
    };
    render(McpServersSettings);
    await waitFor(() => expect(screen.getByTestId("mcp-remove-tiddly")).toBeInTheDocument());

    await fireEvent.click(screen.getByTestId("mcp-remove-tiddly"));
    await waitFor(() =>
      expect(screen.getByTestId("mcp-notice-tiddly")).toHaveTextContent("already in progress"),
    );
    expect(screen.queryByTestId("mcp-load-error")).not.toBeInTheDocument();
    await waitFor(() =>
      expect((screen.getByTestId("mcp-remove-tiddly") as HTMLButtonElement).disabled).toBe(false),
    );
  });

  it("row state does not survive removal onto a re-added same-named provider", async () => {
    providers = [oauthProvider()];
    signInImpl = async () => {
      throw new Error("stale failure");
    };
    render(McpServersSettings);
    await waitFor(() => expect(screen.getByTestId("mcp-sign-in-tiddly")).toBeInTheDocument());

    // Leave a failure notice on the row, then remove it.
    await fireEvent.click(screen.getByTestId("mcp-sign-in-tiddly"));
    await waitFor(() =>
      expect(screen.getByTestId("mcp-notice-tiddly")).toHaveTextContent("stale failure"),
    );
    await fireEvent.click(screen.getByTestId("mcp-remove-tiddly"));
    await waitFor(() => expect(screen.queryByTestId("mcp-row-tiddly")).not.toBeInTheDocument());

    // A new same-named provider starts clean.
    providers = [oauthProvider()];
    eventListeners.get("prompts:synced")?.({ payload: null });
    await waitFor(() => expect(screen.getByTestId("mcp-row-tiddly")).toBeInTheDocument());
    expect(screen.queryByTestId("mcp-notice-tiddly")).not.toBeInTheDocument();
  });

  it("an unknown status discriminant degrades to its raw name, not undefined", async () => {
    providers = [
      oauthProvider({
        has_token: true,
        status: { state: "future_state" } as unknown as McpProviderInfo["status"],
      }),
    ];
    render(McpServersSettings);
    await waitFor(() => expect(screen.getByTestId("mcp-row-tiddly")).toBeInTheDocument());
    expect(screen.getByTestId("mcp-status-tiddly")).toHaveTextContent("future_state");
    expect(screen.getByTestId("mcp-status-tiddly")).toHaveClass("text-muted");
  });
});

describe("McpServersSettings — live-run fixes", () => {
  it("a probe result survives a background sync; only flow notices are transient", async () => {
    // Found in the live run: the sign-in kicked off a background sync, and
    // its prompts:synced event was wiping the Test result the user had just
    // requested. Probe outcomes persist; "Signed in." is the transient one.
    providers = [oauthProvider({ has_token: true, status: { state: "ok", prompt_count: 3 } })];
    render(McpServersSettings);
    await waitFor(() => expect(screen.getByTestId("mcp-test-tiddly")).toBeInTheDocument());

    savedProbeResult = { state: "ok", prompt_count: 4 };
    await fireEvent.click(screen.getByTestId("mcp-test-tiddly"));
    await waitFor(() =>
      expect(screen.getByTestId("mcp-notice-tiddly")).toHaveTextContent("Connected — 4 prompts."),
    );

    eventListeners.get("prompts:synced")?.({ payload: null });
    // Presence must hold after the event-driven refresh settles.
    await waitFor(() =>
      expect(screen.getByTestId("mcp-notice-tiddly")).toHaveTextContent("Connected — 4 prompts."),
    );
  });
});
