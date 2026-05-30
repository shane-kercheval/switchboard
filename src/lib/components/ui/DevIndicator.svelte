<script lang="ts">
  import { ICON_BUTTON_CLASS, ICON_SIZE } from "$lib/components/ui/iconButton";
  import Tooltip from "$lib/components/ui/Tooltip.svelte";

  // `||` not `??`: a detached HEAD yields an empty-string branch, which `??`
  // would pass through verbatim and render as a blank tooltip line.
  const port = import.meta.env.VITE_DEV_PORT || "?";
  const branch = import.meta.env.VITE_GIT_BRANCH || "unknown";
</script>

{#if import.meta.env.DEV}
  <Tooltip side="bottom" delayDuration={0}>
    {#snippet trigger(props)}
      <!-- tabindex=0: a hover/focus-only indicator (no click). bits-ui Trigger
           spreads handler props but doesn't make a <div> focusable on its own,
           so keyboard users couldn't otherwise reach the build detail. Not a
           <button> because a button implies a click action that doesn't exist
           (matches the warning indicator in Sidebar.svelte). -->
      <div
        {...props}
        tabindex="0"
        aria-label="Dev build"
        data-tauri-no-drag
        class={ICON_BUTTON_CLASS}
      >
        <svg
          width={ICON_SIZE}
          height={ICON_SIZE}
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          stroke-width="1.5"
          stroke-linecap="round"
          stroke-linejoin="round"
          aria-hidden="true"
        >
          <polyline points="16 18 22 12 16 6" />
          <polyline points="8 6 2 12 8 18" />
        </svg>
      </div>
    {/snippet}
    <div class="text-[13px] font-medium">Dev build</div>
    <div class="text-primary-fg/70 mt-1 font-mono text-[13px]">{branch} · :{port}</div>
  </Tooltip>
{/if}
