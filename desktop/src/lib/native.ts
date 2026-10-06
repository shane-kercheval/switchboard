// Thin wrappers over Tauri's native-integration plugins, isolated here so
// components depend on a small surface that tests can mock (rather than mocking
// the plugin packages directly).

import { writeText } from "@tauri-apps/plugin-clipboard-manager";
import { open as openDialog } from "@tauri-apps/plugin-dialog";

/// Copy text to the system clipboard.
export async function copyText(text: string): Promise<void> {
  await writeText(text);
}

/// Show the native single-folder picker; returns the chosen path, or `null` if
/// the user cancelled.
export async function pickDirectory(): Promise<string | null> {
  const result = await openDialog({ directory: true, multiple: false });
  return typeof result === "string" ? result : null;
}
