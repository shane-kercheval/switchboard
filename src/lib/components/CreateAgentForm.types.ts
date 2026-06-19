import type { HarnessKind } from "$lib/types";

/// What the create-agent form emits on submit. `model`/`effort` are a
/// **create-only** concern: present (a curated value) when the user picked one,
/// absent when the harness lacks the capability on that axis. Absent ⇒ the
/// backend receives `None` ⇒ no flag is sent (the harness uses its default).
/// Attach carries neither — it brings in an existing session and pins nothing
/// (the harness resumes the session as-is); model/effort are managed afterward
/// from the agent's actions menu. This is enforced here so the rule can't be
/// re-expressed by a caller, not just by the form's submit logic.
export type AgentFormSubmit =
  | { mode: "create"; name: string; harness: HarnessKind; model?: string; effort?: string }
  | { mode: "attach"; name: string; harness: HarnessKind; existingSessionId: string };
