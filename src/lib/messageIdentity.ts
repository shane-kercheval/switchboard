import type { UnifiedRow } from "$lib/state/unified";
import type { HarnessKind } from "$lib/types";

function part(value: string): string {
  return encodeURIComponent(value);
}

export type PinnableMessageIdentity = {
  kind: "pinnable";
  key: string;
  aliases: string[];
  temporary: boolean;
};

export type UnsupportedMessageIdentity = {
  kind: "unsupported";
  reason: string;
};

export type MessageIdentity = PinnableMessageIdentity | UnsupportedMessageIdentity;

const UNSUPPORTED_REASON = "This message has no durable identity and can't be pinned.";
const IMPORTED_REASON =
  "This imported message can't be pinned because its identity does not survive reopening.";
const ANTIGRAVITY_REASON =
  "Antigravity replies can't be pinned because its CLI does not provide a message identity that survives reopening.";

function sendReplyKey(sendId: string, agentId: string): string {
  return `agent:send:${part(sendId)}:${part(agentId)}`;
}

function harnessWillHydrate(harness: HarnessKind | undefined): boolean {
  return harness === "claude_code" || harness === "codex";
}

/// Persistent identity for one rendered message. A hydration key is canonical
/// for agent content because it survives session-file re-parses independently
/// of send correlation. The send key remains an alias for a response pinned
/// before Codex/Claude publishes hydration identity; the pin store migrates
/// that alias atomically once the canonical key appears.
///
/// Imported prompts and permanently keyless agent turns deliberately remain
/// unsupported. Their parser-generated turn ids change on every read, while an
/// author/timestamp fallback can collide and silently resolve to the wrong
/// message — an unavailable pin is safer than a false match.
export function messageIdentityForRow(
  row: Extract<UnifiedRow, { kind: "user" | "agent" }>,
  harness?: HarnessKind,
): MessageIdentity {
  if (row.kind === "user") {
    if (row.send_id === undefined) return { kind: "unsupported", reason: IMPORTED_REASON };
    return {
      kind: "pinnable",
      key: `user:send:${part(row.send_id)}`,
      aliases: [],
      temporary: false,
    };
  }

  // This is intentionally a positive allowlist: absent or future correlation
  // variants must not become authority for persistent aliases by default.
  const trustedSend =
    row.turn.send_correlation === "live" || row.turn.send_correlation === "durable_link";
  const sendAlias =
    row.send_id === undefined || !trustedSend
      ? undefined
      : sendReplyKey(row.send_id, row.turn.agent_id);
  if (row.turn.hydration_key != null) {
    return {
      kind: "pinnable",
      key: `agent:hydration:${part(row.turn.agent_id)}:${part(row.turn.hydration_key)}`,
      aliases: sendAlias === undefined ? [] : [sendAlias],
      temporary: false,
    };
  }
  if (sendAlias !== undefined && row.turn.status === "streaming" && harnessWillHydrate(harness)) {
    return { kind: "pinnable", key: sendAlias, aliases: [], temporary: true };
  }
  return {
    kind: "unsupported",
    reason: harness === "antigravity" ? ANTIGRAVITY_REASON : UNSUPPORTED_REASON,
  };
}

export function identityKeys(identity: PinnableMessageIdentity): string[] {
  return [identity.key, ...identity.aliases];
}
