<script lang="ts">
  /// A nested submenu inside `DropdownMenu` — a row that opens a second menu
  /// beside it rather than performing an action.
  ///
  /// Use this instead of an in-place accordion whenever the nested content is a
  /// *list of choices*: bits-ui keeps the sub-content as its own roving-focus
  /// group, so arrow keys, typeahead, and escape all scope to the level you're
  /// in, and the parent menu doesn't grow a scrollbar because one branch was
  /// expanded.
  ///
  /// `children` render only while the submenu is open (bits-ui mounts
  /// sub-content on demand), which makes `onOpenChange` the natural place to
  /// fetch whatever the submenu lists.
  import type { Snippet } from "svelte";
  import { DropdownMenu as Bits } from "bits-ui";
  import { MENU_CONTENT_CLASS, MENU_ITEM_CLASS } from "$lib/components/ui/menuStyles";
  import { cn } from "$lib/utils";

  type Props = {
    open?: boolean;
    /// The row's own content. Rendered inside the trigger, before the chevron.
    trigger: Snippet;
    /// The submenu's items. Mounted only while open.
    children: Snippet;
    disabled?: boolean;
    class?: string;
    contentClass?: string;
    contentTestid?: string;
    /// Which side the submenu opens toward. Collision detection flips it when
    /// there isn't room, so this is a preference rather than a guarantee.
    ///
    /// Visual only: bits-ui ties the open/close *keys* to text direction, not to
    /// this, so a left-opening submenu still opens on `ArrowRight` and closes on
    /// `ArrowLeft` under the default `ltr`.
    side?: "left" | "right";
    onOpenChange?: (open: boolean) => void;
    [key: `data-${string}`]: string | undefined;
  };

  let {
    open = $bindable(false),
    trigger: renderTrigger,
    children,
    disabled = false,
    class: className,
    contentClass,
    contentTestid,
    side = "right",
    onOpenChange,
    ...rest
  }: Props = $props();
</script>

<Bits.Sub bind:open {onOpenChange}>
  <Bits.SubTrigger {disabled} class={cn(MENU_ITEM_CLASS, "gap-2", className)} {...rest}>
    {@render renderTrigger()}
    <!-- Always the standard right-pointing disclosure chevron, even when the
         submenu opens left: at the trailing edge of a row it reads as "there is
         more here", which a mirrored one does not. -->
    <svg
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      stroke-width="2"
      stroke-linecap="round"
      stroke-linejoin="round"
      class="text-muted ml-auto h-3.5 w-3.5 shrink-0"
      aria-hidden="true"
    >
      <polyline points="9 18 15 12 9 6" />
    </svg>
  </Bits.SubTrigger>
  <Bits.Portal>
    <Bits.SubContent
      {side}
      sideOffset={4}
      data-testid={contentTestid}
      class={cn(MENU_CONTENT_CLASS, contentClass)}
    >
      {@render children()}
    </Bits.SubContent>
  </Bits.Portal>
</Bits.Sub>
