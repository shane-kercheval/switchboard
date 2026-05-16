import type { HarnessKind } from "$lib/types";

export type AgentFormSubmit =
  | { mode: "create"; name: string; harness: HarnessKind }
  | { mode: "attach"; name: string; harness: HarnessKind; existingSessionId: string };
