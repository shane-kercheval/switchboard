import { render } from "vitest-browser-svelte";
import type { AgentRecord } from "$lib/types";
import NavigatorHost from "./NavigatorHost.svelte";

export function mountNavigator(opts: {
  projectId: string;
  agents: AgentRecord[];
}): ReturnType<typeof render> {
  return render(NavigatorHost, { projectId: opts.projectId, agents: opts.agents });
}
