<script lang="ts">
  import Button from "$lib/components/ui/Button.svelte";
  import Input from "$lib/components/ui/Input.svelte";

  type Props = {
    busy?: boolean;
    error?: string | null;
    onSubmit: (name: string) => void;
  };

  let { busy = false, error = null, onSubmit }: Props = $props();
  let name = $state<string>("assistant");
</script>

<div class="flex h-full flex-col items-center justify-center gap-6 p-8">
  <div class="w-full max-w-md space-y-4 rounded-md border border-neutral-200 bg-neutral-50 p-6">
    <div class="space-y-1">
      <h2 class="text-lg font-semibold text-neutral-900">Create an agent</h2>
      <p class="text-sm text-neutral-600">
        Agents live inside the active project. Give it a short name (letters, digits, hyphens,
        underscores).
      </p>
    </div>
    <label class="block space-y-1">
      <span class="text-xs text-neutral-600">Agent name</span>
      <Input bind:value={name} disabled={busy} data-testid="agent-name" />
    </label>
    {#if error}
      <p class="text-xs text-red-700" data-testid="error">{error}</p>
    {/if}
    <div class="flex justify-end">
      <Button
        data-testid="confirm-create-agent"
        disabled={busy || name.trim() === ""}
        onclick={() => onSubmit(name.trim())}
      >
        {busy ? "Creating…" : "Create agent"}
      </Button>
    </div>
  </div>
</div>
