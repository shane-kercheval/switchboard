import type { HarnessKind } from "$lib/types";

/// What the create-agent form emits on submit. `model`/`effort` are the
/// per-agent selection axes: present (a curated value) when the user chose or
/// kept a concrete selection, absent when the picker is on its "keep current"
/// attach default or the harness lacks the capability. Absent ⇒ the backend
/// receives `None` ⇒ no flag is sent (create: harness default; attach: the
/// session's existing model/effort is left untouched).
export type AgentFormSubmit =
  | { mode: "create"; name: string; harness: HarnessKind; model?: string; effort?: string }
  | {
      mode: "attach";
      name: string;
      harness: HarnessKind;
      existingSessionId: string;
      model?: string;
      effort?: string;
    };
