<script lang="ts">
  /// Cycles the theme: system → light → dark → system. Shows the icon for the
  /// current mode (monitor / sun / moon) with the mode in the tooltip.
  import { theme, type ThemeMode } from "$lib/theme.svelte";
  import { ICON_BUTTON_CLASS } from "$lib/components/ui/iconButton";

  const NEXT: Record<ThemeMode, ThemeMode> = {
    system: "light",
    light: "dark",
    dark: "system",
  };

  function cycle(): void {
    theme.set(NEXT[theme.mode]);
  }
</script>

<button
  type="button"
  onclick={cycle}
  title={`Theme: ${theme.mode}`}
  aria-label={`Theme: ${theme.mode}. Click to change.`}
  data-testid="theme-toggle"
  class={ICON_BUTTON_CLASS}
>
  {#if theme.mode === "light"}
    <!-- sun -->
    <svg
      width="16"
      height="16"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      stroke-width="2"
      stroke-linecap="round"
      stroke-linejoin="round"
      aria-hidden="true"
    >
      <circle cx="12" cy="12" r="4" />
      <path
        d="M12 2v2M12 20v2M4.93 4.93l1.41 1.41M17.66 17.66l1.41 1.41M2 12h2M20 12h2M6.34 17.66l-1.41 1.41M19.07 4.93l-1.41 1.41"
      />
    </svg>
  {:else if theme.mode === "dark"}
    <!-- moon -->
    <svg
      width="16"
      height="16"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      stroke-width="2"
      stroke-linecap="round"
      stroke-linejoin="round"
      aria-hidden="true"
    >
      <path d="M21 12.79A9 9 0 1 1 11.21 3 7 7 0 0 0 21 12.79z" />
    </svg>
  {:else}
    <!-- monitor (system) -->
    <svg
      width="16"
      height="16"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      stroke-width="2"
      stroke-linecap="round"
      stroke-linejoin="round"
      aria-hidden="true"
    >
      <rect x="2" y="3" width="20" height="14" rx="2" />
      <path d="M8 21h8M12 17v4" />
    </svg>
  {/if}
</button>
