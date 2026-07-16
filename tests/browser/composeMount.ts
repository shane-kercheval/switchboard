// Mount helpers for compose-surface specs, deliberately SEPARATE from ./mount:
// ComposeBar's module graph pulls in `@tauri-apps/api/webview` (drag-drop),
// which imports more of `@tauri-apps/api/event` (`TauriEvent`, `once`, …) than
// the transcript specs' minimal `{ listen }` mock provides — importing these
// helpers from the shared mount module would break every other spec's import
// graph. Compose specs must also mock `@tauri-apps/api/webview` at their own
// top level (see compose-autosize.browser.test.ts).
import { render } from "vitest-browser-svelte";
import type { AgentRecord, Prompt } from "$lib/types";
import ComposeBar from "$lib/components/ComposeBar.svelte";
import PromptComposerHost from "./PromptComposerHost.svelte";
import PanesWithComposerHost from "./PanesWithComposerHost.svelte";

/** Mount the real `ComposeBar` — its autosize textarea at its real max-height cap. */
export function mountComposeBar(opts: {
  projectId: string;
  agents: AgentRecord[];
}): ReturnType<typeof render> {
  return render(ComposeBar, { props: { projectId: opts.projectId, agents: opts.agents } });
}

/** Mount the pane row above a real `ComposeBar`, wired like `App` (pane Cmd+click
 * → composer focus), so the no-flicker behavior can be exercised end to end. */
export function mountPanesWithComposer(opts: {
  projectId: string;
  agents: AgentRecord[];
  width?: number;
}): ReturnType<typeof render> {
  return render(PanesWithComposerHost, {
    props: {
      projectId: opts.projectId,
      agents: opts.agents,
      ...(opts.width !== undefined ? { width: opts.width } : {}),
    },
  });
}

/** Mount `PromptComposer` via a host that owns `$state` args, as the real parent does. */
export function mountPromptComposer(opts: {
  prompt: Prompt;
  args: Record<string, string>;
  appendedText?: string;
}): ReturnType<typeof render> {
  return render(PromptComposerHost, {
    props: { prompt: opts.prompt, args: opts.args, appendedText: opts.appendedText ?? "" },
  });
}
