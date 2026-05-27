// Synchronous Markdown → sanitized HTML for the transcript. Parsing runs on
// every streaming tick for the growing text segment (see Markdown.svelte), so
// the whole pipeline is deliberately synchronous: marked with no async
// extensions, Prism's synchronous highlighter, and DOMPurify. An async parse
// model would reintroduce the streaming render races this design avoids.

import { Marked, type Tokens } from "marked";
import DOMPurify from "dompurify";
import Prism from "prismjs";

// Prism's core auto-highlights the document on DOMContentLoaded unless told
// otherwise; we only ever call `Prism.highlight` by hand, so disable it.
Prism.manual = true;

// Built into Prism core: markup/html, css, clike, javascript. The rest are
// imported explicitly (no CDN autoloader — this is an offline desktop app, and
// the autoloader fetches grammars over the network). Order matters where a
// grammar extends another: jsx/typescript before tsx.
import "prismjs/components/prism-rust";
import "prismjs/components/prism-typescript";
import "prismjs/components/prism-jsx";
import "prismjs/components/prism-tsx";
import "prismjs/components/prism-python";
import "prismjs/components/prism-bash";
import "prismjs/components/prism-json";
import "prismjs/components/prism-yaml";
import "prismjs/components/prism-toml";
import "prismjs/components/prism-sql";
import "prismjs/components/prism-diff";
import "prismjs/components/prism-markdown";

function escapeHtml(text: string): string {
  return text
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");
}

// `Prism.highlight` is undefined-behavior (and throws on some builds) when the
// grammar is missing, so guard on `Prism.languages[lang]` and fall back to
// escaped plain text — an unknown or unlabeled fence renders as monospace,
// never an error mid-stream.
function highlightCode(code: string, lang: string): string {
  const grammar = lang ? Prism.languages[lang] : undefined;
  if (grammar) {
    return Prism.highlight(code, grammar, lang);
  }
  return escapeHtml(code);
}

const marked = new Marked({ gfm: true, breaks: true });

// Custom code renderer: emit the highlighted block wrapped in our own chrome
// (language badge + Copy button) as part of the HTML string. The chrome is NOT
// injected after render — Markdown.svelte re-renders the whole `{@html}` on each
// streaming tick, which would wipe any post-render DOM. The Copy button sits
// *outside* `<code>` so its label isn't captured when the delegated handler
// copies `code.textContent`.
marked.use({
  renderer: {
    code({ text, lang }: Tokens.Code): string {
      const language = (lang ?? "").match(/\S*/)?.[0] ?? "";
      const highlighted = highlightCode(text, language);
      const label = escapeHtml(language || "text");
      const langClass = language ? ` class="language-${escapeHtml(language)}"` : "";
      return (
        `<div class="md-code-block">` +
        `<div class="md-code-header">` +
        `<span class="md-code-lang">${label}</span>` +
        `<button type="button" class="md-code-copy" aria-label="Copy code">` +
        `<svg class="md-copy-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><rect x="9" y="9" width="11" height="11" rx="2.5"></rect><path d="M14 9V5.5A2.5 2.5 0 0 0 11.5 3H5.5A2.5 2.5 0 0 0 3 5.5V11.5A2.5 2.5 0 0 0 5.5 14H9"></path></svg>` +
        `<svg class="md-check-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><path d="M20 6 9 17l-5-5"></path></svg>` +
        `</button>` +
        `</div>` +
        `<pre${langClass}><code${langClass}>${highlighted}</code></pre>` +
        `</div>`
      );
    },
  },
});

/// Parse Markdown and sanitize to a safe HTML string for `{@html}`.
///
/// DOMPurify's default profile preserves `class` on standard elements, which is
/// all the chrome and Prism token spans need (`language-*`, `token …`,
/// `md-code-*`) — keep that in mind before tightening the config: stripping
/// `class` would silently kill highlighting and the copy-button hook. No custom
/// `ADD_ATTR` is required because the copy handler reads `code.textContent`
/// rather than a stashed `data-` attribute.
export function renderMarkdown(text: string): string {
  const html = marked.parse(text, { async: false });
  return DOMPurify.sanitize(html);
}
