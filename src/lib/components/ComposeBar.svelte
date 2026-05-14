<script lang="ts">
  import Button from "$lib/components/ui/Button.svelte";
  import Textarea from "$lib/components/ui/Textarea.svelte";

  type Props = {
    disabled?: boolean;
    onSubmit: (prompt: string) => void;
  };

  let { disabled = false, onSubmit }: Props = $props();
  let prompt = $state<string>("");

  function handleSubmit(): void {
    const trimmed = prompt.trim();
    if (trimmed === "" || disabled) return;
    onSubmit(trimmed);
    prompt = "";
  }

  function handleKey(event: KeyboardEvent): void {
    // Cmd+Enter submits. Shift+Enter inserts a newline (default textarea behavior).
    if (event.key === "Enter" && event.metaKey) {
      event.preventDefault();
      handleSubmit();
    }
  }
</script>

<div class="border-t border-neutral-200 bg-neutral-50 p-3">
  <div class="flex gap-2">
    <Textarea
      data-testid="compose-textarea"
      placeholder="Type a message…  (⌘+Enter to send)"
      rows={3}
      bind:value={prompt}
      {disabled}
      onkeydown={handleKey}
    />
    <Button
      data-testid="compose-send"
      onclick={handleSubmit}
      disabled={disabled || prompt.trim() === ""}
    >
      Send
    </Button>
  </div>
</div>
