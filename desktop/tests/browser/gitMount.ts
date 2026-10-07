import { render } from "vitest-browser-svelte";
import GitViewHost from "./GitViewHost.svelte";

/** Mount the complete Git view against the viewport for split-layout checks. */
export function mountGitView(): ReturnType<typeof render> {
  return render(GitViewHost);
}
