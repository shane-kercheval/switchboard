<script lang="ts">
  import { Bot, FolderOpen } from "@lucide/svelte";
  import Button from "$lib/components/ui/Button.svelte";
  import GettingStarted from "$lib/components/GettingStarted.svelte";

  let { onAddProject }: { onAddProject: () => void } = $props();
</script>

<div class="mx-auto flex w-full max-w-xl flex-col gap-8">
  <header class="flex flex-col items-center gap-3 text-center">
    <h1 class="text-fg text-3xl font-semibold">Switchboard</h1>
    <p class="text-muted text-sm leading-relaxed">
      One window for your AI coding CLIs — Claude Code, Codex, and more. Fan a task out to several
      agents at once, relay results from one to another, and steer their work side by side — without
      juggling a terminal per session.
    </p>
    <Button data-testid="welcome-add-project" onclick={onAddProject} class="mt-1">
      Add project
    </Button>
  </header>

  <ul class="flex flex-col gap-6">
    <li class="flex gap-3">
      <FolderOpen
        size={18}
        strokeWidth={1.8}
        aria-hidden="true"
        class="text-muted mt-0.5 shrink-0"
      />
      <div class="flex min-w-0 flex-col gap-1">
        <p class="text-fg text-sm font-medium">Start with a project</p>
        <p class="text-muted text-sm leading-relaxed">
          A project is bound to a working directory — a repo, or any folder you want to work in.
          Everything the project does happens inside that folder; Switchboard keeps its own state in
          a <code class="bg-panel text-fg rounded px-1 font-mono text-xs">.switchboard/</code>
          subdirectory there. One folder can hold more than one project, so separate efforts can run against
          the same codebase.
        </p>
      </div>
    </li>
    <li class="flex gap-3">
      <Bot size={18} strokeWidth={1.8} aria-hidden="true" class="text-muted mt-0.5 shrink-0" />
      <div class="flex min-w-0 flex-col gap-1">
        <p class="text-fg text-sm font-medium">Agents are your CLIs</p>
        <p class="text-muted text-sm leading-relaxed">
          An agent is one CLI running in the project's directory, holding its own ongoing
          conversation. Switchboard starts and drives the CLI for you, using the install, sign-in,
          and configuration you already have — it never replaces or modifies the CLI itself. A
          project can run several agents side by side — even two of the same CLI — each with its own
          private conversation, all working in the same files.
        </p>
      </div>
    </li>
    <GettingStarted />
  </ul>
</div>
