// Shared prompt-mode rules used by both the composer's Preview and its send, so
// the preview shows exactly what the agent will receive — they must never
// diverge. Kept tiny and framework-free for direct unit testing.

import type { Prompt } from "$lib/types";

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
