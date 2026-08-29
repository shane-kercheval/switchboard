import {
  listMessagePins,
  migrateMessagePin as persistMessagePinMigration,
  removeMessagePins as persistMessagePinRemoval,
  setMessagePin as persistMessagePin,
} from "$lib/api";
import { identityKeys, type PinnableMessageIdentity } from "$lib/messageIdentity";
import type { MessagePin, ProjectId } from "$lib/types";

type PinIntent = {
  id: number;
  kind: "pin";
  key: string;
  pinnedAt: string;
};

type UnpinIntent = {
  id: number;
  kind: "unpin";
  keys: string[];
};

type MigrateIntent = {
  id: number;
  kind: "migrate";
  fromKey: string;
  toKey: string;
};

type PendingIntent = PinIntent | UnpinIntent | MigrateIntent;
type PinAuthority = "initial" | "loading" | "trusted" | "unknown";
type MigrationRelation = { fromKey: string; toKey: string };

type ProjectPins = {
  confirmed: MessagePin[];
  pending: PendingIntent[];
  collapsed: Record<string, boolean>;
  authority: PinAuthority;
  loadError: string | null;
  mutationError: string | null;
  nextIntentId: number;
  migrationAttempts: Record<string, "pending" | "succeeded" | "failed">;
  indeterminateMigrations: Record<string, MigrationRelation>;
};

const byProject = $state<Record<ProjectId, ProjectPins>>({});
// Promise serialization is non-reactive control flow; components observe only
// `byProject`, so plain maps avoid exposing queue churn as UI state.
// eslint-disable-next-line svelte/prefer-svelte-reactivity
const queues = new Map<ProjectId, Promise<void>>();
// eslint-disable-next-line svelte/prefer-svelte-reactivity
const loads = new Map<ProjectId, Promise<void>>();
// Scroll offsets are mount-time snapshots, not reactive message state. Keeping
// them outside `byProject` avoids repainting the Pins tree on every scroll.
// eslint-disable-next-line svelte/prefer-svelte-reactivity
const scrollTops = new Map<ProjectId, number>();

function registerLoad(projectId: ProjectId, load: Promise<void>): Promise<void> {
  loads.set(projectId, load);
  void load.finally(() => {
    if (loads.get(projectId) === load) loads.delete(projectId);
  });
  return load;
}

function ensureState(projectId: ProjectId): ProjectPins {
  let state = byProject[projectId];
  if (!state) {
    byProject[projectId] = {
      confirmed: [],
      pending: [],
      collapsed: {},
      authority: "initial",
      loadError: null,
      mutationError: null,
      nextIntentId: 0,
      migrationAttempts: {},
      indeterminateMigrations: {},
    };
    state = byProject[projectId];
  }
  return state;
}

function applyIntent(pins: MessagePin[], intent: PendingIntent): MessagePin[] {
  if (intent.kind === "migrate") {
    if (!pins.some((pin) => pin.key === intent.fromKey)) return pins;
    if (pins.some((pin) => pin.key === intent.toKey)) {
      return pins.filter((pin) => pin.key !== intent.fromKey);
    }
    return pins.map((pin) => (pin.key === intent.fromKey ? { ...pin, key: intent.toKey } : pin));
  }
  if (intent.kind === "pin") {
    return pins.some((pin) => pin.key === intent.key)
      ? pins
      : [...pins, { key: intent.key, pinned_at: intent.pinnedAt }];
  }
  return pins.filter((pin) => !intent.keys.includes(pin.key));
}

function visiblePins(state: ProjectPins): MessagePin[] {
  return state.pending.reduce(applyIntent, state.confirmed);
}

function enqueue(projectId: ProjectId, run: () => Promise<void>): Promise<void> {
  const prior = queues.get(projectId) ?? Promise.resolve();
  const next = prior.catch(() => undefined).then(run);
  queues.set(projectId, next);
  void next.finally(() => {
    if (queues.get(projectId) === next) queues.delete(projectId);
  });
  return next;
}

function removeIntent(state: ProjectPins, id: number): void {
  state.pending = state.pending.filter((intent) => intent.id !== id);
}

function normalizeMigrationCollapse(
  state: ProjectPins,
  relation: MigrationRelation,
  pins: MessagePin[],
): void {
  const value = state.collapsed[relation.toKey] ?? state.collapsed[relation.fromKey];
  const hasCanonical = pins.some((pin) => pin.key === relation.toKey);
  const hasAlias = pins.some((pin) => pin.key === relation.fromKey);
  delete state.collapsed[relation.fromKey];
  delete state.collapsed[relation.toKey];
  if (value === undefined) return;
  if (hasCanonical) state.collapsed[relation.toKey] = value;
  else if (hasAlias) state.collapsed[relation.fromKey] = value;
}

function acceptAuthoritativePins(state: ProjectPins, pins: MessagePin[]): void {
  state.confirmed = pins;
  for (const relation of Object.values(state.indeterminateMigrations)) {
    normalizeMigrationCollapse(state, relation, pins);
  }
  state.indeterminateMigrations = {};
  state.authority = "trusted";
  state.loadError = null;
}

function recoverAuthoritativePins(
  projectId: ProjectId,
  state: ProjectPins,
  saveError: unknown,
  surfaceMutationError: boolean,
): Promise<void> {
  const message = String(saveError);
  state.authority = "loading";
  const recovery = (async (): Promise<void> => {
    try {
      const persisted = await listMessagePins(projectId);
      acceptAuthoritativePins(state, Array.isArray(persisted) ? persisted : []);
      if (surfaceMutationError) state.mutationError = message;
      else console.warn("[switchboard] automatic pin identity migration failed", saveError);
    } catch (reloadError) {
      state.authority = "unknown";
      state.loadError = `${message}; unable to reload pins: ${String(reloadError)}`;
      // Once persistence authority is lost, its retryable error supersedes an
      // older mutation error; showing both would offer competing next steps.
      state.mutationError = null;
    }
  })();
  return registerLoad(projectId, recovery);
}

function queueSet(projectId: ProjectId, key: string): void {
  const state = ensureState(projectId);
  const intent: PinIntent = {
    id: ++state.nextIntentId,
    kind: "pin",
    key,
    pinnedAt: new Date().toISOString(),
  };
  state.pending = [...state.pending, intent];

  void enqueue(projectId, async () => {
    try {
      const persisted = await persistMessagePin(projectId, key, true);
      acceptAuthoritativePins(state, Array.isArray(persisted) ? persisted : []);
    } catch (error) {
      await recoverAuthoritativePins(projectId, state, error, true);
    } finally {
      removeIntent(state, intent.id);
    }
  });
}

function relatedMigrationKeys(state: ProjectPins, keys: string[]): string[] {
  // Synchronous scratch state; no reactive consumer can observe its mutations.
  // eslint-disable-next-line svelte/prefer-svelte-reactivity
  const related = new Set(keys);
  let changed = true;
  while (changed) {
    changed = false;
    for (const intent of state.pending) {
      if (intent.kind !== "migrate") continue;
      if (!related.has(intent.fromKey) && !related.has(intent.toKey)) continue;
      if (!related.has(intent.fromKey)) {
        related.add(intent.fromKey);
        changed = true;
      }
      if (!related.has(intent.toKey)) {
        related.add(intent.toKey);
        changed = true;
      }
    }
    for (const relation of Object.values(state.indeterminateMigrations)) {
      if (!related.has(relation.fromKey) && !related.has(relation.toKey)) continue;
      if (!related.has(relation.fromKey)) {
        related.add(relation.fromKey);
        changed = true;
      }
      if (!related.has(relation.toKey)) {
        related.add(relation.toKey);
        changed = true;
      }
    }
  }
  return [...related];
}

function queueRemove(projectId: ProjectId, requestedKeys: string[]): void {
  const state = ensureState(projectId);
  const keys = relatedMigrationKeys(state, requestedKeys);
  const intent: UnpinIntent = {
    id: ++state.nextIntentId,
    kind: "unpin",
    keys,
  };
  state.pending = [...state.pending, intent];
  for (const key of keys) delete state.collapsed[key];

  void enqueue(projectId, async () => {
    try {
      const persisted = await persistMessagePinRemoval(projectId, keys);
      acceptAuthoritativePins(state, Array.isArray(persisted) ? persisted : []);
    } catch (error) {
      await recoverAuthoritativePins(projectId, state, error, true);
    } finally {
      removeIntent(state, intent.id);
    }
  });
}

function migrationAttemptKey(fromKey: string, toKey: string): string {
  return `${fromKey}\u0000${toKey}`;
}

function queueMigration(projectId: ProjectId, fromKey: string, toKey: string): void {
  const state = ensureState(projectId);
  const attemptKey = migrationAttemptKey(fromKey, toKey);
  if (state.migrationAttempts[attemptKey] !== undefined) return;
  state.migrationAttempts[attemptKey] = "pending";
  const intent: MigrateIntent = {
    id: ++state.nextIntentId,
    kind: "migrate",
    fromKey,
    toKey,
  };
  state.pending = [...state.pending, intent];
  if (state.collapsed[fromKey] !== undefined) {
    state.collapsed[toKey] = state.collapsed[fromKey];
  }

  void enqueue(projectId, async () => {
    try {
      const persisted = await persistMessagePinMigration(projectId, fromKey, toKey);
      const authoritative = Array.isArray(persisted) ? persisted : [];
      acceptAuthoritativePins(state, authoritative);
      normalizeMigrationCollapse(state, { fromKey, toKey }, authoritative);
      state.migrationAttempts[attemptKey] = "succeeded";
    } catch (error) {
      state.migrationAttempts[attemptKey] = "failed";
      state.indeterminateMigrations[attemptKey] = { fromKey, toKey };
      await recoverAuthoritativePins(projectId, state, error, false);
    } finally {
      removeIntent(state, intent.id);
    }
  });
}

export function pinsFor(projectId: ProjectId): MessagePin[] {
  const state = byProject[projectId];
  return state === undefined ? [] : visiblePins(state);
}

export function pinsLoaded(projectId: ProjectId): boolean {
  return byProject[projectId]?.authority === "trusted";
}

export function pinsLoading(projectId: ProjectId): boolean {
  return byProject[projectId]?.authority === "loading";
}

export function pinsUnavailableReason(projectId: ProjectId): string {
  return byProject[projectId]?.authority === "unknown"
    ? "Pins are unavailable until reloaded."
    : "Loading pins…";
}

export function pinLoadError(projectId: ProjectId): string | null {
  return byProject[projectId]?.loadError ?? null;
}

export function pinMutationError(projectId: ProjectId): string | null {
  return byProject[projectId]?.mutationError ?? null;
}

export function dismissPinMutationError(projectId: ProjectId): void {
  ensureState(projectId).mutationError = null;
}

export function isMessagePinned(projectId: ProjectId, identity: PinnableMessageIdentity): boolean {
  const keys = identityKeys(identity);
  return pinsFor(projectId).some((pin) => keys.includes(pin.key));
}

export function isPinCollapsed(projectId: ProjectId, key: string): boolean {
  return byProject[projectId]?.collapsed[key] ?? false;
}

export function togglePinCollapsed(projectId: ProjectId, key: string): void {
  const state = ensureState(projectId);
  state.collapsed[key] = !(state.collapsed[key] ?? false);
}

export function setPinsCollapsed(projectId: ProjectId, keys: string[], collapsed: boolean): void {
  const state = ensureState(projectId);
  for (const key of keys) state.collapsed[key] = collapsed;
}

export function pinsScrollTopFor(projectId: ProjectId): number {
  return scrollTops.get(projectId) ?? 0;
}

export function setPinsScrollTop(projectId: ProjectId, scrollTop: number): void {
  if (!Number.isFinite(scrollTop)) return;
  scrollTops.set(projectId, Math.max(0, scrollTop));
}

export async function loadMessagePins(projectId: ProjectId, force = false): Promise<void> {
  const state = ensureState(projectId);
  const active = loads.get(projectId);
  if (active !== undefined) return active;
  // Exported entry points are called from reactive effects that subscribe to
  // authority. Reads must remain single-flight when their own transitions
  // re-enter those effects.
  if (state.authority === "loading") return;
  if (state.authority === "trusted" && !force) return;
  if (state.authority === "unknown" && !force) return;

  state.authority = "loading";
  state.loadError = null;
  const load = enqueue(projectId, async () => {
    try {
      const persisted = await listMessagePins(projectId);
      acceptAuthoritativePins(state, Array.isArray(persisted) ? persisted : []);
    } catch (error) {
      state.authority = "unknown";
      state.loadError = String(error);
    }
  });
  return registerLoad(projectId, load);
}

export function setMessagePinned(
  projectId: ProjectId,
  identity: PinnableMessageIdentity,
  pinned: boolean,
): void {
  const state = ensureState(projectId);
  if (state.authority !== "trusted") return;
  const present = pinsFor(projectId).map((pin) => pin.key);
  if (pinned) {
    if (!identityKeys(identity).some((key) => present.includes(key)))
      queueSet(projectId, identity.key);
    return;
  }
  const keys = identityKeys(identity);
  if (keys.some((key) => present.includes(key))) queueRemove(projectId, keys);
}

export function setStoredPinPinned(projectId: ProjectId, key: string, pinned: boolean): void {
  const state = ensureState(projectId);
  if (state.authority !== "trusted") return;
  const present = pinsFor(projectId).some((pin) => pin.key === key);
  if (present === pinned) return;
  if (pinned) queueSet(projectId, key);
  else queueRemove(projectId, [key]);
}

export function removeStoredMessagePins(projectId: ProjectId, keys: string[]): void {
  const state = ensureState(projectId);
  if (state.authority !== "trusted") return;
  const present = pinsFor(projectId).map((pin) => pin.key);
  const removals = keys.filter(
    (key, index) => keys.indexOf(key) === index && present.includes(key),
  );
  if (removals.length > 0) queueRemove(projectId, removals);
}

export function toggleMessagePin(projectId: ProjectId, identity: PinnableMessageIdentity): void {
  setMessagePinned(projectId, identity, !isMessagePinned(projectId, identity));
}

export function reconcileMessagePinIdentities(
  projectId: ProjectId,
  identities: PinnableMessageIdentity[],
): void {
  const state = ensureState(projectId);
  if (state.authority !== "trusted") return;
  // Synchronous scratch indexes; their contents never escape this call.
  // eslint-disable-next-line svelte/prefer-svelte-reactivity
  const present = new Set(pinsFor(projectId).map((pin) => pin.key));
  // eslint-disable-next-line svelte/prefer-svelte-reactivity
  const candidates = new Map<string, Set<string>>();
  for (const identity of identities) {
    if (identity.temporary) continue;
    for (const alias of identity.aliases) {
      if (!present.has(alias)) continue;
      // eslint-disable-next-line svelte/prefer-svelte-reactivity
      const canonical = candidates.get(alias) ?? new Set<string>();
      canonical.add(identity.key);
      candidates.set(alias, canonical);
    }
  }
  for (const [alias, canonical] of candidates) {
    if (canonical.size !== 1) continue;
    const toKey = [...canonical][0];
    if (toKey !== undefined) queueMigration(projectId, alias, toKey);
  }
}

export const _testing = {
  reset(): void {
    for (const key of Object.keys(byProject)) delete byProject[key];
    queues.clear();
    loads.clear();
    scrollTops.clear();
  },
};
