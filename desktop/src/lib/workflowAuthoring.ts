export const WORKFLOW_AUTHORING_GUIDE_URL =
  "https://raw.githubusercontent.com/shane-kercheval/switchboard/refs/heads/main/docs/agent-instructions/workflows.md";

export function workflowAuthoringPrompt(workflowsDir: string | null): string {
  const destination = workflowsDir ?? "the Switchboard workflows folder shown in Settings";

  return `Help me create a custom workflow for Switchboard.

Read and follow this workflow authoring guide:
${WORKFLOW_AUTHORING_GUIDE_URL}

First, ask me what I want the workflow to accomplish. Clarify which work should run in parallel or sequentially, which outputs should be forwarded or combined, and which values I should choose when starting it.

Then create a valid workflow and save it in:
${destination}

Use only currently runnable syntax documented in the guide; do not use features it marks as gated or not yet runnable. Follow the guide's authoring conventions and validation rules. If you cannot access the guide, stop and tell me rather than guessing. If you can read the guide but cannot write to that folder, give me the proposed filename and complete YAML instead.`;
}
