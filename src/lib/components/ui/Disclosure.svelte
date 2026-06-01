<script lang="ts">
  /// A collapsible panel: a rotating-chevron summary row above a body that the
  /// `<details>` element shows/hides. Width-capped so wide content (tool I/O,
  /// reasoning) doesn't sprawl the full transcript width.
  ///
  /// Two modes:
  /// - Uncontrolled (omit `open`): the native `<details>` toggle drives it,
  ///   collapsed by default. Used where the panel just opens and closes.
  /// - Controlled (pass `open` + `ontoggle`): the consumer owns the open state,
  ///   so it can survive external changes (e.g. a tool's running→complete
  ///   transition won't yank a panel the user opened). The native toggle is
  ///   suppressed; `ontoggle` fires on summary activation for the consumer to
  ///   flip its own state.
  import type { Snippet } from "svelte";

  type Props = {
    /// Controlled open state. Pass this ONLY together with `ontoggle` —
    /// supplying `open` without `ontoggle` suppresses the native toggle but has
    /// no callback to flip the state, locking the panel open or shut. Omit both
    /// for uncontrolled (native-toggle) mode.
    open?: boolean;
    ontoggle?: () => void;
    testid?: string;
    header: Snippet;
    children: Snippet;
    [key: `data-${string}`]: string | undefined;
  };

  let { open = undefined, ontoggle, testid, header, children, ...rest }: Props = $props();

  const controlled = $derived(open !== undefined);

  function onsummaryclick(event: Event): void {
    if (!controlled) return;
    // Controlled mode requires `ontoggle`; without it the suppressed native
    // toggle leaves the panel stuck. Surface that loudly in dev rather than
    // letting a new consumer ship a frozen panel.
    if (import.meta.env.DEV && !ontoggle) {
      console.error("Disclosure: `open` was passed without `ontoggle` — the panel will be stuck");
    }
    // preventDefault stops the native toggle from double-applying on top of the
    // consumer-driven `open`.
    event.preventDefault();
    ontoggle?.();
  }
</script>

<details
  class="bg-panel/35 group/disclosure max-w-[600px] rounded-md text-xs"
  data-testid={testid}
  open={controlled ? open : undefined}
  {...rest}
>
  <summary
    class="text-fg flex min-h-8 cursor-pointer list-none items-center gap-2 px-2.5 py-1.5 marker:hidden"
    onclick={onsummaryclick}
  >
    <span
      class="text-muted flex h-4 w-4 shrink-0 items-center justify-center transition-transform group-open/disclosure:rotate-90"
      aria-hidden="true">›</span
    >
    {@render header()}
  </summary>
  {@render children()}
</details>
