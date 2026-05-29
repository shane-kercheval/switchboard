<script lang="ts">
  import Button from "$lib/components/ui/Button.svelte";
  import GettingStarted from "$lib/components/GettingStarted.svelte";

  let { onNewProject, onAddExisting }: { onNewProject: () => void; onAddExisting: () => void } =
    $props();
</script>

<div class="flex w-full flex-col gap-8">
  <header class="flex flex-col items-center gap-2 text-center">
    <h1 class="text-fg text-3xl font-semibold">Switchboard</h1>
    <p class="text-muted max-w-xl text-sm leading-relaxed">
      A desktop app for human orchestration of AI coding CLIs. Run several CLIs — Claude Code,
      Codex, and more — together in one project: fan a task out to all of them, relay results from
      one to another, and stay in the loop, steering and comparing their work — without juggling a
      separate terminal for each.
    </p>
  </header>

  <section class="border-border bg-panel/40 flex flex-col gap-4 rounded-lg border p-5">
    <div class="flex flex-col gap-1.5">
      <h2 class="text-fg text-sm font-semibold">Projects</h2>
      <p class="text-muted text-sm leading-relaxed">
        A project is bound to a working directory — your repo, or any folder you want to work in.
        Switchboard keeps the project's state under a
        <code class="bg-panel text-fg rounded px-1 font-mono text-xs">.switchboard/</code>
        directory there, and everything in the project happens inside that folder. A single folder can
        hold more than one project, so you can run separate efforts against the same codebase.
      </p>
    </div>
    <div class="flex gap-2">
      <Button size="sm" data-testid="welcome-new-project" onclick={onNewProject}>New project</Button
      >
      <Button
        variant="secondary"
        size="sm"
        data-testid="welcome-add-existing"
        onclick={onAddExisting}
      >
        Add existing
      </Button>
    </div>
  </section>

  <section class="border-border bg-panel/40 flex flex-col gap-1.5 rounded-lg border p-5">
    <h2 class="text-fg text-sm font-semibold">Agents</h2>
    <p class="text-muted text-sm leading-relaxed">
      An agent is a single AI coding session inside a project: one tool — a CLI such as Claude Code
      or Codex — running in the project's directory, with its own ongoing conversation. Switchboard
      invokes these CLIs directly; it doesn't replace or modify them, so each agent runs under the
      sign-in you already use. A project can run several agents at once, even two on the same tool,
      each keeping its own conversation while sharing the same files.
    </p>
    <GettingStarted />
  </section>
</div>
