<script lang="ts">
  /// Renders a Markdown string as sanitized, syntax-highlighted HTML. One
  /// instance per text segment (see UnifiedTranscript): the parse runs in a
  /// `$derived` keyed on `text`, so during streaming only the growing segment's
  /// instance re-parses — completed segments keep a stable `text` prop and never
  /// re-run. That structural memoization is why there's no manual parse cache.
  ///
  /// Code-block chrome (language badge + Copy button) is part of the parsed HTML
  /// string, not injected after render — `{@html}` replaces the whole subtree on
  /// each re-parse, so any post-render DOM mutation would be wiped. Interactions
  /// are handled by one delegated listener on the stable container instead.
  import { renderMarkdown } from "$lib/markdown";
  import { copyText } from "$lib/native";
  import { openExternalUrl } from "$lib/api";
  import { cn } from "$lib/utils";

  let { text = "", class: className = "" }: { text?: string; class?: string } = $props();

  // Known limitation: while a segment is still streaming, the whole segment
  // re-parses (and re-highlights) every token, so a partially-typed token in the
  // live code block can briefly change color as more characters arrive. It's
  // confined to the one growing segment (completed segments are stable) and
  // Prism is fast, so it's minor. If it ever reads as objectionable, the fallback
  // is to render the live segment's code as plain monospace and highlight only
  // once finalized — not built pre-emptively.
  const html = $derived(renderMarkdown(text));

  // Per-button reset timers (keyed on the button element) so copying one block
  // doesn't cancel another block's "Copied → Copy" reset. WeakMap doesn't pin
  // the nodes, so detached buttons are collected normally.
  const copyTimers = new WeakMap<Element, ReturnType<typeof setTimeout>>();

  function delegate(node: HTMLElement) {
    function onClick(event: MouseEvent): void {
      const target = event.target as HTMLElement | null;
      if (!target) return;

      const copyBtn = target.closest(".md-code-copy");
      if (copyBtn) {
        // Copy the exact source from the rendered <code>: Prism only wraps text
        // in spans, so textContent reconstructs the original verbatim (no
        // escaping round-trip, no duplicated raw-source node).
        const code = copyBtn.closest(".md-code-block")?.querySelector("code");
        if (code) {
          // Confirm only after the clipboard write resolves — showing "Copied"
          // optimistically would lie if the write rejected (stale paste).
          void copyText(code.textContent ?? "")
            .then(() => {
              copyBtn.setAttribute("data-copied", "true");
              clearTimeout(copyTimers.get(copyBtn));
              copyTimers.set(
                copyBtn,
                setTimeout(() => copyBtn.removeAttribute("data-copied"), 1000),
              );
            })
            .catch((err: unknown) => {
              console.error("[switchboard] copy failed", err);
            });
        }
        return;
      }

      const anchor = target.closest("a");
      if (anchor) {
        // Never let a link navigate the app's own webview; hand it to the OS
        // (which validates the scheme) instead.
        event.preventDefault();
        const href = anchor.getAttribute("href");
        if (href) {
          void openExternalUrl(href).catch((err: unknown) => {
            console.error("[switchboard] open link failed", err);
          });
        }
      }
    }
    node.addEventListener("click", onClick);
    return {
      destroy() {
        node.removeEventListener("click", onClick);
      },
    };
  }
</script>

<!-- eslint-disable-next-line svelte/no-at-html-tags -- `html` is DOMPurify-sanitized in renderMarkdown -->
<div class={cn("markdown-body", className)} use:delegate>{@html html}</div>
