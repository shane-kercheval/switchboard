// Thin wrappers over Tauri's native-integration plugins, isolated here so
// components depend on a small surface that tests can mock (rather than mocking
// the plugin packages directly).

import { writeText } from "@tauri-apps/plugin-clipboard-manager";

/// Copy text to the system clipboard.
export async function copyText(text: string): Promise<void> {
  await writeText(text);
}
