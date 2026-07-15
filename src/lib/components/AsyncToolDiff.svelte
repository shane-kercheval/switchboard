<script lang="ts">
  import DiffView from "$lib/components/DiffView.svelte";
  import { synthesizeEditDiffAsync, synthesizeMcpTextEditDiffAsync } from "$lib/toolDiff";
  import type { ExpandedDiffCoordinator } from "$lib/toolDiff";
  import type { EditedFile, FileDiff } from "$lib/types";

  let {
    sourceKind,
    file,
    before = "",
    after = "",
    contentTruncated = false,
    coordinator,
    language,
    testid,
  }: {
    sourceKind: "file" | "mcp";
    file?: EditedFile;
    before?: string;
    after?: string;
    contentTruncated?: boolean;
    coordinator: ExpandedDiffCoordinator;
    language: string;
    testid: string;
  } = $props();

  let diff = $state<FileDiff | null>(null);
  let loading = $state(true);
  let unavailable = $state(false);

  $effect(() => {
    const kind = sourceKind;
    const currentFile = file;
    const currentBefore = before;
    const currentAfter = after;
    const currentContentTruncated = contentTruncated;
    const currentCoordinator = coordinator;
    const abortController = new AbortController();
    let cancelled = false;
    diff = null;
    loading = true;
    unavailable = false;

    const pending = currentCoordinator.run(
      (timeoutMs) =>
        kind === "file" && currentFile !== undefined
          ? synthesizeEditDiffAsync(currentFile, timeoutMs)
          : synthesizeMcpTextEditDiffAsync(
              currentBefore,
              currentAfter,
              currentContentTruncated,
              timeoutMs,
            ),
      abortController.signal,
    );

    void pending
      .then((result) => {
        if (cancelled) return;
        diff = result ?? null;
        unavailable = result === undefined;
        loading = false;
      })
      .catch(() => {
        if (cancelled) return;
        unavailable = true;
        loading = false;
      });

    return () => {
      cancelled = true;
      abortController.abort();
    };
  });
</script>

{#if loading}
  <p class="text-muted py-2 text-[11px]" role="status" data-testid={`${testid}-loading`}>
    Preparing full diff…
  </p>
{:else if diff !== null}
  <div class="border-border/60 overflow-hidden rounded border" data-testid={testid}>
    <DiffView {diff} style="unified" {language} compact />
  </div>
{:else if unavailable}
  <p class="text-muted py-2 text-[11px]" data-testid={`${testid}-unavailable`}>
    This edit is too complex to render inline.
  </p>
{/if}
