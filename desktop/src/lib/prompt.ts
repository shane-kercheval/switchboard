// Shared prompt-mode rules used by both the composer's Preview and its send, so
// the preview shows exactly what the agent will receive — they must never
// diverge. Kept tiny and framework-free for direct unit testing.

import type { Prompt } from "$lib/types";

/// The reserved provider prefix for the app-owned, read-only built-in library.
/// Mirrors Rust `BUILTIN_PROVIDER`; `provider === BUILTIN_PROVIDER` is the wire
/// contract for "this is a read-only built-in".
export const BUILTIN_PROVIDER = "builtin";

/// The reserved provider prefix for the user's local file-based prompt store.
/// Mirrors Rust `LOCAL_PROVIDER`. Any other provider value is an MCP server's
/// registered name.
export const LOCAL_PROVIDER = "local";

/// Whether a provider is one Switchboard resolves locally (built-in library or
/// the local file store) rather than a remote MCP server. Local providers can
/// surface a prompt's template; MCP providers cannot (the protocol renders
/// server-side), which is why a null preview source means different things for
/// each — a broken/deleted local prompt vs. an MCP prompt with no fetchable body.
export function isLocalProvider(provider: string): boolean {
  return provider === LOCAL_PROVIDER || provider === BUILTIN_PROVIDER;
}

/// Whether a prompt is an app-owned read-only built-in (vs. the user's own local
/// or an MCP prompt). Built-ins are tagged read-only in the picker and offer a
/// "Copy to my prompts" action instead of in-place editing.
export function isBuiltinPrompt(prompt: Pick<Prompt, "provider">): boolean {
  return prompt.provider === BUILTIN_PROVIDER;
}

/// Split a `provider:name` prompt id into its parts at the **first** colon (the
/// provider prefix is a reserved slug that never contains one; a name
/// theoretically could). Returns null when there is no colon or when either half
/// is empty — matching Rust `PromptId::parse`, which rejects empty parts. Callers
/// treat null as an unresolvable id.
export function parsePromptId(id: string): { provider: string; name: string } | null {
  const i = id.indexOf(":");
  if (i <= 0 || i === id.length - 1) return null;
  return { provider: id.slice(0, i), name: id.slice(i + 1) };
}

/// The label to show a user for a prompt: its friendly `title` when the server
/// provides one, falling back to the `name` slug (local prompts, or MCP servers
/// that omit a title).
export function promptDisplayName(prompt: Pick<Prompt, "title" | "name">): string {
  return prompt.title ?? prompt.name;
}

/// Combine a server-rendered prompt with the user's appended free text: the
/// rendered prompt, a blank line, then the appended text. Appended text is
/// trimmed; when empty the result is the rendered prompt alone (no trailing
/// blank line). This is the single definition of the "what the agent receives"
/// string — Preview and send both call it.
export function combinePromptMessage(rendered: string, appendedText: string): string {
  const appended = appendedText.trim();
  return appended === "" ? rendered : `${rendered}\n\n${appended}`;
}

/// The names of required arguments left blank (after trimming). Send is blocked
/// while this is non-empty; the composer highlights these fields. Optional
/// arguments may be empty — the server applies its own defaults.
export function missingRequiredArgs(prompt: Prompt, args: Record<string, string>): string[] {
  return prompt.arguments
    .filter((a) => a.required && (args[a.name] ?? "").trim() === "")
    .map((a) => a.name);
}

/// Build the `render_prompt` arguments map from the declared schema: include
/// each argument whose value is non-blank (after trimming), and **omit** the
/// rest. Blank optionals must be absent — not sent as `""` — so the server
/// applies its own default/conditional (per the MCP convention; the backend only
/// omits the whole object when the map is empty). Preview and Send both call this
/// so what's previewed is exactly what's sent. Non-blank values are passed
/// verbatim (internal whitespace preserved).
export function buildRenderArgs(
  prompt: Prompt,
  args: Record<string, string>,
): Record<string, string> {
  const out: Record<string, string> = {};
  for (const arg of prompt.arguments) {
    const value = args[arg.name] ?? "";
    if (value.trim() !== "") out[arg.name] = value;
  }
  return out;
}
