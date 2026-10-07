import type { AgentSelection, HarnessKind } from "$lib/types";

/// What the create-agent form emits on submit. Create persists the complete
/// independent selection configuration atomically. Attach carries none: it
/// resumes the existing session without pinning model or effort.
export type AgentFormSubmit =
  | {
      mode: "create";
      name: string;
      harness: HarnessKind;
      selection: AgentSelection;
    }
  | { mode: "attach"; name: string; harness: HarnessKind; existingSessionId: string };
