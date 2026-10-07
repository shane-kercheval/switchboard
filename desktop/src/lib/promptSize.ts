import type { Attachment } from "$lib/types";

/// Most bytes a message may carry and still be passed to a harness CLI as a
/// command-line argument. Mirrors `MAX_PROMPT_BYTES` in
/// `crates/harness/src/adapter.rs`, which is the authority: the dispatcher
/// refuses anything over it. This copy exists so the compose bar can refuse
/// *before* it clears the draft, keeping the user's text in the box. A Rust test
/// reads this file and fails if the two literals drift.
export const MAX_PROMPT_BYTES = 768 * 1024;

/// The agent-facing footer the dispatcher appends for attachments, byte-for-byte
/// as `render_prompt_with_attachments` in `crates/core/src/attachment.rs` does,
/// so the size measured here is the size the backend measures.
function attachmentFooter(attachments: readonly Pick<Attachment, "label" | "path">[]): string {
  if (attachments.length === 0) return "";
  let footer = "\n\n---\nAttached files (read them):";
  for (const attachment of attachments) footer += `\n${attachment.label}: ${attachment.path}`;
  return footer;
}

export function utf8ByteLength(text: string): number {
  return new TextEncoder().encode(text).length;
}

/// The refusal for a message of `bytes`, or `null` when it fits. Phrased to
/// follow a prefix such as "Not sent: " and to match the backend's own refusal.
export function oversizedPromptMessage(bytes: number): string | null {
  if (bytes <= MAX_PROMPT_BYTES) return null;
  return `message is ${Math.floor(bytes / 1024)} KB; the agent CLI accepts at most ${MAX_PROMPT_BYTES / 1024} KB per message (macOS limits command-line arguments to 1 MB). Attach the text as a file instead.`;
}

/// Whether `text` plus its attachment footer is too large to dispatch; the
/// refusal message when it is, `null` when it fits. Cheap on the common path:
/// a UTF-16 code unit encodes to at most three UTF-8 bytes, so text that can't
/// possibly reach the limit is never encoded.
export function promptTooLarge(
  text: string,
  attachments: readonly Pick<Attachment, "label" | "path">[],
): string | null {
  const dispatched = text + attachmentFooter(attachments);
  if (dispatched.length * 3 <= MAX_PROMPT_BYTES) return null;
  return oversizedPromptMessage(utf8ByteLength(dispatched));
}
