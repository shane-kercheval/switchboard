import { beforeEach, describe, expect, it, vi } from "vitest";
import { waitFor } from "@testing-library/svelte";
import type { PinnableMessageIdentity } from "$lib/messageIdentity";
import type { MessagePin } from "$lib/types";

const PROJECT = "project-a";
const identity = (key: string, aliases: string[] = []): PinnableMessageIdentity => ({
  kind: "pinnable",
  key,
  aliases,
  temporary: false,
});

let persisted: MessagePin[] = [];
let failedKeys = new Set<string>();
let deferredLoad: Promise<MessagePin[]> | undefined;
let migrationFailure = false;
let migrationCommitsThenFails = false;
let migrationGate: Promise<void> | undefined;
let listFailure = false;
let queuedListResponses: Array<MessagePin[] | Promise<MessagePin[]> | Error> = [];

const invokeMock = vi.fn(
  async (cmd: string, args?: Record<string, unknown>): Promise<MessagePin[]> => {
    if (cmd === "list_message_pins") {
      const queued = queuedListResponses.shift();
      if (queued instanceof Error) throw queued;
      if (queued !== undefined) return await queued;
      if (listFailure) throw new Error("pins read failed");
      return deferredLoad ?? [...persisted];
    }
    if (cmd === "set_message_pin") {
      const key = args?.key as string;
      if (failedKeys.has(key)) throw new Error(`disk full for ${key}`);
      if (args?.pinned === true && !persisted.some((pin) => pin.key === key)) {
        persisted.push({ key, pinned_at: "2026-08-07T12:00:00Z" });
      } else if (args?.pinned === false) {
        persisted = persisted.filter((pin) => pin.key !== key);
      }
      return [...persisted];
    }
    if (cmd === "remove_message_pins") {
      const keys = new Set(args?.keys as string[]);
      for (const key of keys) {
        if (failedKeys.has(key)) throw new Error(`disk full for ${key}`);
      }
      persisted = persisted.filter((pin) => !keys.has(pin.key));
      return [...persisted];
    }
    if (cmd === "migrate_message_pin") {
      if (migrationGate !== undefined) await migrationGate;
      const fromKey = args?.fromKey as string;
      const toKey = args?.toKey as string;
      if (migrationFailure) throw new Error("migration disk full");
      persisted = persisted.map((pin) => (pin.key === fromKey ? { ...pin, key: toKey } : pin));
      if (migrationCommitsThenFails) throw new Error("directory sync failed");
      return [...persisted];
    }
    throw new Error(`unexpected command ${cmd}`);
  },
);

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: Record<string, unknown>) => invokeMock(cmd, args),
}));

const pins = await import("./messagePins.svelte");

beforeEach(() => {
  pins._testing.reset();
  persisted = [];
  failedKeys = new Set();
  deferredLoad = undefined;
  migrationFailure = false;
  migrationCommitsThenFails = false;
  migrationGate = undefined;
  listFailure = false;
  queuedListResponses = [];
  invokeMock.mockClear();
});

describe("message pins state", () => {
  it("removes a batch of stored pins once without touching other pins", async () => {
    persisted = [
      { key: "message-a", pinned_at: "2026-08-07T12:00:00Z" },
      { key: "message-b", pinned_at: "2026-08-07T12:01:00Z" },
    ];
    await pins.loadMessagePins(PROJECT);

    pins.removeStoredMessagePins(PROJECT, ["message-a", "message-a", "missing"]);

    expect(pins.pinsFor(PROJECT).map((pin) => pin.key)).toEqual(["message-b"]);
    await waitFor(() => expect(persisted.map((pin) => pin.key)).toEqual(["message-b"]));
    expect(invokeMock).toHaveBeenCalledWith("remove_message_pins", {
      projectId: PROJECT,
      keys: ["message-a"],
    });
  });

  it("does not interpret a click as a toggle before the initial state is known", async () => {
    let resolveLoad!: (value: MessagePin[]) => void;
    deferredLoad = new Promise((resolve) => (resolveLoad = resolve));
    const load = pins.loadMessagePins(PROJECT);
    await waitFor(() => expect(invokeMock).toHaveBeenCalledTimes(1));

    pins.toggleMessagePin(PROJECT, identity("message-a"));
    expect(invokeMock).toHaveBeenCalledTimes(1);
    expect(pins.pinsFor(PROJECT)).toEqual([]);

    resolveLoad([{ key: "message-a", pinned_at: "2026-08-07T11:00:00Z" }]);
    await load;
    expect(pins.isMessagePinned(PROJECT, identity("message-a"))).toBe(true);
  });

  it("serializes rapid changes and preserves collapse state only while pinned", async () => {
    await pins.loadMessagePins(PROJECT);
    pins.setMessagePinned(PROJECT, identity("message-a"), true);
    pins.togglePinCollapsed(PROJECT, "message-a");
    expect(pins.isPinCollapsed(PROJECT, "message-a")).toBe(true);
    pins.setMessagePinned(PROJECT, identity("message-a"), false);

    await waitFor(() => expect(invokeMock).toHaveBeenCalledTimes(3));
    await waitFor(() => expect(persisted).toEqual([]));
    expect(pins.pinsFor(PROJECT)).toEqual([]);
    expect(pins.isPinCollapsed(PROJECT, "message-a")).toBe(false);
  });

  it("keeps an earlier mutation failure visible while a later mutation succeeds", async () => {
    await pins.loadMessagePins(PROJECT);
    failedKeys.add("message-a");
    pins.setMessagePinned(PROJECT, identity("message-a"), true);
    pins.setMessagePinned(PROJECT, identity("message-b"), true);

    await waitFor(() => expect(persisted.map((pin) => pin.key)).toEqual(["message-b"]));
    expect(pins.pinsFor(PROJECT).map((pin) => pin.key)).toEqual(["message-b"]);
    expect(pins.pinMutationError(PROJECT)).toContain("disk full for message-a");
  });

  it("replays a pending later intent over an authoritative recovery snapshot", async () => {
    await pins.loadMessagePins(PROJECT);
    failedKeys.add("message-a");
    pins.setMessagePinned(PROJECT, identity("message-a"), true);
    pins.setMessagePinned(PROJECT, identity("message-b"), true);

    expect(pins.pinsFor(PROJECT).map((pin) => pin.key)).toEqual(["message-a", "message-b"]);
    await waitFor(() => expect(pins.pinsFor(PROJECT).map((pin) => pin.key)).toEqual(["message-b"]));
  });

  it("migrates a temporary send alias to durable hydration identity atomically", async () => {
    persisted = [{ key: "agent:send:send-a:agent-a", pinned_at: "2026-08-07T12:00:00Z" }];
    await pins.loadMessagePins(PROJECT);
    const durable = identity("agent:hydration:agent-a:message-a", ["agent:send:send-a:agent-a"]);

    pins.reconcileMessagePinIdentities(PROJECT, [durable]);

    await waitFor(() =>
      expect(pins.pinsFor(PROJECT).map((pin) => pin.key)).toEqual([
        "agent:hydration:agent-a:message-a",
      ]),
    );
    expect(invokeMock.mock.calls.some(([command]) => command === "migrate_message_pin")).toBe(true);
    expect(pins.isMessagePinned(PROJECT, durable)).toBe(true);
  });

  it("attempts a failed migration once and preserves an unpin made while it is pending", async () => {
    const alias = "agent:send:send-a:agent-a";
    const canonical = "agent:hydration:agent-a:message-a";
    persisted = [{ key: alias, pinned_at: "2026-08-07T12:00:00Z" }];
    await pins.loadMessagePins(PROJECT);
    let releaseMigration!: () => void;
    migrationGate = new Promise((resolve) => (releaseMigration = resolve));
    migrationFailure = true;
    const durable = identity(canonical, [alias]);

    pins.reconcileMessagePinIdentities(PROJECT, [durable]);
    expect(pins.pinsFor(PROJECT).map((pin) => pin.key)).toEqual([canonical]);
    pins.setMessagePinned(PROJECT, durable, false);
    expect(pins.pinsFor(PROJECT)).toEqual([]);
    releaseMigration();

    await waitFor(() => expect(persisted).toEqual([]));
    pins.reconcileMessagePinIdentities(PROJECT, [durable]);
    pins.reconcileMessagePinIdentities(PROJECT, [durable]);
    const migrationCalls = invokeMock.mock.calls.filter(
      ([command]) => command === "migrate_message_pin",
    );
    expect(migrationCalls).toHaveLength(1);
    expect(pins.pinsFor(PROJECT)).toEqual([]);
    expect(pins.pinMutationError(PROJECT)).toBeNull();
  });

  it("locks mutations until an indeterminate migration is reloaded authoritatively", async () => {
    const alias = "agent:send:send-a:agent-a";
    const canonical = "agent:hydration:agent-a:message-a";
    persisted = [{ key: alias, pinned_at: "2026-08-07T12:00:00Z" }];
    await pins.loadMessagePins(PROJECT);
    failedKeys.add("earlier-user-pin");
    pins.setMessagePinned(PROJECT, identity("earlier-user-pin"), true);
    await waitFor(() => expect(pins.pinMutationError(PROJECT)).toContain("earlier-user-pin"));
    pins.togglePinCollapsed(PROJECT, alias);
    migrationCommitsThenFails = true;
    listFailure = true;

    pins.reconcileMessagePinIdentities(PROJECT, [identity(canonical, [alias])]);

    await waitFor(() => expect(pins.pinLoadError(PROJECT)).toContain("pins read failed"));
    expect(persisted.map((pin) => pin.key)).toEqual([canonical]);
    expect(pins.pinsLoaded(PROJECT)).toBe(false);
    expect(pins.pinsUnavailableReason(PROJECT)).toBe("Pins are unavailable until reloaded.");
    expect(pins.isPinCollapsed(PROJECT, canonical)).toBe(true);
    expect(pins.pinMutationError(PROJECT)).toBeNull();

    pins.setStoredPinPinned(PROJECT, alias, false);
    expect(invokeMock.mock.calls.some(([command]) => command === "remove_message_pins")).toBe(
      false,
    );

    listFailure = false;
    await pins.loadMessagePins(PROJECT, true);
    expect(pins.pinsLoaded(PROJECT)).toBe(true);
    expect(pins.isPinCollapsed(PROJECT, canonical)).toBe(true);
    expect(pins.isPinCollapsed(PROJECT, alias)).toBe(false);

    pins.setStoredPinPinned(PROJECT, canonical, false);
    await waitFor(() => expect(persisted).toEqual([]));
  });

  it("shares an in-flight recovery read with reactive load re-entry", async () => {
    const alias = "agent:send:send-a:agent-a";
    const canonical = "agent:hydration:agent-a:message-a";
    persisted = [{ key: alias, pinned_at: "2026-08-07T12:00:00Z" }];
    await pins.loadMessagePins(PROJECT);
    migrationFailure = true;
    let resolveRecovery!: (pins: MessagePin[]) => void;
    const recovery = new Promise<MessagePin[]>((resolve) => (resolveRecovery = resolve));
    queuedListResponses = [recovery, new Error("redundant read should not run")];

    pins.reconcileMessagePinIdentities(PROJECT, [identity(canonical, [alias])]);
    await waitFor(() =>
      expect(
        invokeMock.mock.calls.filter(([command]) => command === "list_message_pins"),
      ).toHaveLength(2),
    );

    const concurrentRetry = pins.loadMessagePins(PROJECT, true);
    resolveRecovery([...persisted]);
    await concurrentRetry;

    expect(
      invokeMock.mock.calls.filter(([command]) => command === "list_message_pins"),
    ).toHaveLength(2);
    expect(pins.pinsLoaded(PROJECT)).toBe(true);
    expect(pins.pinLoadError(PROJECT)).toBeNull();
  });

  it("restores collapse state to the persisted alias after a recovered migration failure", async () => {
    const alias = "agent:send:send-a:agent-a";
    const canonical = "agent:hydration:agent-a:message-a";
    persisted = [{ key: alias, pinned_at: "2026-08-07T12:00:00Z" }];
    await pins.loadMessagePins(PROJECT);
    pins.togglePinCollapsed(PROJECT, alias);
    migrationFailure = true;

    pins.reconcileMessagePinIdentities(PROJECT, [identity(canonical, [alias])]);

    await waitFor(() => expect(pins.pinsFor(PROJECT).map((pin) => pin.key)).toEqual([alias]));
    expect(pins.isPinCollapsed(PROJECT, alias)).toBe(true);
    expect(pins.isPinCollapsed(PROJECT, canonical)).toBe(false);
    expect(pins.pinMutationError(PROJECT)).toBeNull();
  });

  it("does not migrate an alias that resolves to multiple canonical messages", async () => {
    const alias = "agent:send:send-a:agent-a";
    persisted = [{ key: alias, pinned_at: "2026-08-07T12:00:00Z" }];
    await pins.loadMessagePins(PROJECT);

    pins.reconcileMessagePinIdentities(PROJECT, [
      identity("agent:hydration:agent-a:first", [alias]),
      identity("agent:hydration:agent-a:second", [alias]),
    ]);

    expect(invokeMock.mock.calls.some(([command]) => command === "migrate_message_pin")).toBe(
      false,
    );
    expect(pins.pinsFor(PROJECT).map((pin) => pin.key)).toEqual([alias]);
  });

  it("bulk-updates only the supplied cards' collapse state", async () => {
    await pins.loadMessagePins(PROJECT);
    pins.setPinsCollapsed(PROJECT, ["message-a", "message-b"], true);
    expect(pins.isPinCollapsed(PROJECT, "message-a")).toBe(true);
    expect(pins.isPinCollapsed(PROJECT, "message-b")).toBe(true);
    expect(pins.isPinCollapsed(PROJECT, "message-c")).toBe(false);
    pins.setPinsCollapsed(PROJECT, ["message-a", "message-b"], false);
    expect(pins.isPinCollapsed(PROJECT, "message-a")).toBe(false);
    expect(pins.isPinCollapsed(PROJECT, "message-b")).toBe(false);
  });
});
