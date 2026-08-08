<script module lang="ts">
  type FreshHoverWindowSubscriber = () => void;

  let freshHoverWindowSubscribers: FreshHoverWindowSubscriber[] = [];
  let freshHoverListenersActive = false;
  let awaitingInputAfterWindowFocus = false;

  function notifyFreshHoverWindowSubscribers(): void {
    awaitingInputAfterWindowFocus = true;
    for (const subscriber of [...freshHoverWindowSubscribers]) subscriber();
  }

  function noteFreshHoverInput(): void {
    awaitingInputAfterWindowFocus = false;
  }

  function handleFreshHoverVisibilityChange(): void {
    if (document.visibilityState === "hidden") notifyFreshHoverWindowSubscribers();
  }

  function startFreshHoverListeners(): void {
    if (freshHoverListenersActive) return;
    freshHoverListenersActive = true;
    window.addEventListener("pointerdown", noteFreshHoverInput, true);
    window.addEventListener("keydown", noteFreshHoverInput, true);
    window.addEventListener("blur", notifyFreshHoverWindowSubscribers);
    document.addEventListener("visibilitychange", handleFreshHoverVisibilityChange);
  }

  function stopFreshHoverListeners(): void {
    if (!freshHoverListenersActive) return;
    freshHoverListenersActive = false;
    window.removeEventListener("pointerdown", noteFreshHoverInput, true);
    window.removeEventListener("keydown", noteFreshHoverInput, true);
    window.removeEventListener("blur", notifyFreshHoverWindowSubscribers);
    document.removeEventListener("visibilitychange", handleFreshHoverVisibilityChange);
    awaitingInputAfterWindowFocus = false;
  }

  function registerFreshHoverWindowSubscriber(subscriber: FreshHoverWindowSubscriber): () => void {
    freshHoverWindowSubscribers.push(subscriber);
    startFreshHoverListeners();
    return () => {
      freshHoverWindowSubscribers = freshHoverWindowSubscribers.filter(
        (candidate) => candidate !== subscriber,
      );
      if (freshHoverWindowSubscribers.length === 0) stopFreshHoverListeners();
    };
  }

  function focusAfterWindowChangeAllowed(): boolean {
    // An OS-level refocus can preserve :focus-visible on the element that had
    // keyboard focus. Require fresh input so that restoration does not reopen
    // a tooltip while genuine keyboard navigation still does.
    return !awaitingInputAfterWindowFocus;
  }
</script>

<script lang="ts">
  /// Custom dark tooltip wrapping `bits-ui` Tooltip, so callers get hover/focus
  /// open, delay, dismissal, and ARIA for free instead of the bare native
  /// `title`. The trigger delegates to the caller's own element via the
  /// `trigger` snippet (spread `props` onto it), so there's no wrapper element
  /// and no nested button.
  ///
  /// **Two content modes** (discriminated union — TS rejects passing neither
  /// or both, so a caller can't accidentally render an empty tooltip):
  /// - **Label mode**: `label="..."` renders the single-line bold title.
  ///   Optional `shortcut="⌘↵"` line shown beneath it.
  /// - **Children mode**: the default slot owns the entire content area —
  ///   the caller styles its own layout (rows, lists, etc.). `shortcut` is
  ///   ignored in this mode (the caller's children own the visual stack).
  import type { Snippet } from "svelte";
  import { createAttachmentKey, type Attachment } from "svelte/attachments";
  import { Tooltip as Bits, Portal } from "bits-ui";

  type ReopenBehavior = "default" | "fresh-hover";

  type Common = {
    side?: "top" | "bottom" | "left" | "right";
    delayDuration?: number;
    skipDelayDuration?: number;
    disableHoverableContent?: boolean;
    disabled?: boolean;
    ignoreNonKeyboardFocus?: boolean;
    /// Actionable state controls close on activation and stay quiet until the
    /// pointer genuinely leaves and re-enters. They also use the full hover
    /// delay on every entry instead of the recent-tooltip grace period.
    reopen?: ReopenBehavior;
    open?: boolean;
    /// Receives the bits-ui trigger props — spread them onto your element.
    trigger: Snippet<[Record<string, unknown>]>;
  };
  type LabelProps = Common & {
    label: string;
    /// Keyboard-shortcut hint shown beneath the label (label mode only).
    shortcut?: string;
    children?: never;
  };
  type ChildrenProps = Common & {
    children: Snippet;
    label?: never;
    shortcut?: never;
  };
  type Props = LabelProps | ChildrenProps;

  let {
    side = "top",
    delayDuration = 700,
    skipDelayDuration = 300,
    disableHoverableContent = false,
    disabled = false,
    ignoreNonKeyboardFocus = undefined,
    reopen = "default",
    open = $bindable(false),
    trigger,
    ...rest
  }: Props = $props();

  const freshHoverAttachmentKey = createAttachmentKey();
  // `reopen` can change while this component remains mounted, so the public
  // close handle must already exist when fresh-hover behavior attaches.
  const tether = Bits.createTether();
  let activationSuppressed = false;
  let windowSuppressed = false;
  let pointerInside = false;

  type EventHandler = (event: Event) => void;

  function asEventHandler(value: unknown): EventHandler | undefined {
    return typeof value === "function" ? (value as EventHandler) : undefined;
  }

  function closeAndSuppressAfterActivation(): void {
    tether.close();
    activationSuppressed = pointerInside;
  }

  function suppressForWindowChange(): void {
    tether.close();
    windowSuppressed = true;
  }

  const freshHoverAttachment: Attachment<HTMLElement> = (node) => {
    activationSuppressed = false;
    windowSuppressed = false;
    pointerInside = node.matches(":hover");
    const unregisterWindowSubscriber = registerFreshHoverWindowSubscriber(suppressForWindowChange);
    // Callers spread trigger props before declaring their own `onclick`, which
    // would replace a composed handler. Capture closes first without competing
    // with the caller's activation handler.
    node.addEventListener("click", closeAndSuppressAfterActivation, true);

    return () => {
      node.removeEventListener("click", closeAndSuppressAfterActivation, true);
      unregisterWindowSubscriber();
      tether.close();
      activationSuppressed = false;
      windowSuppressed = false;
      pointerInside = false;
    };
  };

  function handlePointerEnter(event: PointerEvent, bitsHandler: EventHandler | undefined): void {
    const freshEntry = !pointerInside;
    pointerInside = true;
    if (windowSuppressed && freshEntry) windowSuppressed = false;
    if (!activationSuppressed && !windowSuppressed) bitsHandler?.(event);
  }

  function handlePointerMove(event: PointerEvent, bitsHandler: EventHandler | undefined): void {
    if (!activationSuppressed && !windowSuppressed) bitsHandler?.(event);
  }

  function handlePointerLeave(event: PointerEvent, bitsHandler: EventHandler | undefined): void {
    bitsHandler?.(event);
    activationSuppressed = false;
    windowSuppressed = false;
    pointerInside = false;
  }

  function handleFocus(event: FocusEvent, bitsHandler: EventHandler | undefined): void {
    if (focusAfterWindowChangeAllowed()) bitsHandler?.(event);
  }

  function freshHoverTriggerProps(props: Record<string, unknown>): Record<PropertyKey, unknown> {
    if (reopen !== "fresh-hover") return props;

    const {
      onclick: _onclick,
      onpointerenter,
      onpointermove,
      onpointerleave,
      onfocus,
      ...rest
    } = props;
    return {
      ...rest,
      onpointerenter: (event: PointerEvent) =>
        handlePointerEnter(event, asEventHandler(onpointerenter)),
      onpointermove: (event: PointerEvent) =>
        handlePointerMove(event, asEventHandler(onpointermove)),
      onpointerleave: (event: PointerEvent) =>
        handlePointerLeave(event, asEventHandler(onpointerleave)),
      onfocus: (event: FocusEvent) => handleFocus(event, asEventHandler(onfocus)),
      [freshHoverAttachmentKey]: freshHoverAttachment,
    };
  }
</script>

<Bits.Provider {delayDuration} skipDelayDuration={reopen === "fresh-hover" ? 0 : skipDelayDuration}>
  <Bits.Root
    bind:open
    {tether}
    {disableHoverableContent}
    {disabled}
    ignoreNonKeyboardFocus={ignoreNonKeyboardFocus ?? reopen === "fresh-hover"}
  >
    <Bits.Trigger {tether}>
      {#snippet child({ props })}
        {@render trigger(freshHoverTriggerProps(props))}
      {/snippet}
    </Bits.Trigger>
    {#if !disabled}
      <Portal>
        <Bits.Content
          {side}
          sideOffset={6}
          data-testid="tooltip-content"
          class="bg-primary text-primary-fg z-50 rounded-lg px-2.5 py-1.5 shadow-[0_10px_28px_rgba(0,0,0,0.20)]"
        >
          <Bits.Arrow class="fill-primary" />
          {#if rest.children}
            {@render rest.children()}
          {:else}
            <div class="text-[13px] font-medium">{rest.label}</div>
            {#if rest.shortcut}
              <div class="text-primary-fg/70 mt-1 font-mono text-[13px]">{rest.shortcut}</div>
            {/if}
          {/if}
        </Bits.Content>
      </Portal>
    {/if}
  </Bits.Root>
</Bits.Provider>
