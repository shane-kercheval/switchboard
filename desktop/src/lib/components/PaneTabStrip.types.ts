import type { TranscriptPane } from "$lib/state/transcriptPanes.svelte";

export type HeaderPaneState = "visible" | "minimized" | "behind_maximized";

export type HeaderPaneEntry = {
  pane: TranscriptPane;
  state: HeaderPaneState;
};
