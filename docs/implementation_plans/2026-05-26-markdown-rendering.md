# Markdown & code rendering in the unified transcript

**Status:** proposed · **Date:** 2026-05-26

## Context & motivation

Today the transcript renders every message body as raw text in a
`whitespace-pre-wrap` div (`UnifiedTranscript.svelte` — the user-message body at
the `userMessage` snippet and the agent text item in `turnBody`). LLMs respond
in Markdown, so users currently read literal `**bold**`, `- list`, `# heading`,
and triple-backtick fences with no syntax highlighting. This makes long
explanations and code exhausting to read and makes the app feel unfinished.

This plan adds Markdown rendering (including fenced code with syntax
highlighting and a per-block copy button) for **both** user-typed messages and
agent responses, while preserving the one structural property the transcript
already gets right: **tool calls are not embedded inside a text blob.** A turn
is a list of `items` where text segments and tool-call items interleave
(`turnBody` in `UnifiedTranscript.svelte`). So Markdown is rendered
**per text segment**, and tool-call components continue to render between
segments exactly as they do now. The only thing that does not survive is a
Markdown construct that spans a tool call (e.g. a list interrupted mid-list by a
tool use) — each text segment parses independently. That is acceptable and
matches how the harness streams.

### Text vs. thinking items (relationship to M4.10)

A "text segment" here is any item with `item_kind === "text"` (`types.ts:172`).
That item carries a second discriminator, `kind: ContentKind` where
`ContentKind = "text" | "thinking"` (`types.ts:26`): `text` is answer prose,
`thinking` is model reasoning (Gemini `thoughts[]`, Antigravity `thinking`,
Claude extended-thinking). **Today both render identically** through the single
`{#if item.item_kind === "text"}` branch in `turnBody` — neither is formatted.

This plan ships **before M4.10** ("Thinking/reasoning block rendering"), so it
applies `<Markdown>` to **all** `item_kind === "text"` items — answer *and*
thinking — which simply formats both, matching their current
rendered-identically behavior. The collapsible/muted/labeled treatment that
visually subordinates reasoning is **M4.10's job, not this plan's.**

The convergence contract that keeps the two plans from clobbering each other:

- **`Markdown.svelte` is content-agnostic** — it renders a string and knows
  nothing about `text` vs `thinking`. This is what lets M4.10 reuse it.
- **M4.10 lands second** and will *split* the `item_kind === "text"` branch on
  `kind`: `text` stays as it is after this plan; `thinking` moves into M4.10's
  collapsible container — whose **body still renders through `<Markdown>`**. So
  M4.10 relocates the thinking case and wraps it; it does not replace the
  Markdown rendering this plan introduces.
- Net effect for the M4.10 implementer: reasoning prose stays formatted, just
  inside a collapsible. The container is M4.10's; the content formatter is this
  plan's.

### The load-bearing architectural decision (read before implementing)

**Each text segment renders its own `<Markdown text={…}>` instance, and parsing
lives in a `$derived` keyed on that instance's `text` prop.** This is what makes
streaming cheap: during a streaming turn only the *last, growing* segment's
`text` changes, so Svelte re-runs the parse for that one component only.
Completed segments keep a referentially-stable `text` prop and never re-parse.
We get per-segment memoization structurally — there is **no** manual parse cache,
**no** `MutationObserver`, and **no** post-render DOM mutation.

This is a deliberate rejection of the alternative sketched in the original
proposal doc, which used a single `{@html}` block plus a `MutationObserver` that
re-injected copy-button DOM after every render. That pattern is the *opposite*
of high-performance under streaming: Svelte replaces the whole `innerHTML` on
each token, wiping the injected DOM, so the observer re-scans every `<pre>` and
re-creates header/button elements on every token — and observing the node it
mutates risks a feedback loop. We avoid all of it by producing the complete
HTML (highlighting included) in the parse step and handling interactions with
**event delegation** on a stable container.

### Library choices (and what they were chosen over)

- **`marked`** for Markdown→HTML. Synchronous and fast, which is exactly what a
  per-token re-parse of the streaming tail needs. Degrades gracefully on
  incomplete input (an unclosed ``` fence renders as a code block; a half-typed
  `[link` renders as text), so live streaming doesn't throw or corrupt the DOM.
  Chosen over a `unified`/`remark`/`rehype` AST pipeline (heavier, async parsing
  model — race-prone and flickery under streaming) and over hand-rolled regex
  parsing (unmaintainable once tables/nested lists/edge cases are in scope).
- **`marked-highlight`** to plug highlighting into the parse step, calling a
  **synchronous** highlight function that returns an HTML string. This is what
  keeps highlighting *inside* the produced HTML rather than a post-render DOM
  pass.
- **Prism** (`prismjs`) as the highlighter. Synchronous and **class-based**
  (`<span class="token keyword">…`), so the theme is plain CSS mapped to our
  semantic tokens and re-themes instantly on light/dark with no JS — the same
  property the rest of the token model relies on. Chosen over **Shiki** (highest
  fidelity but async + WASM, and it emits **inline-style** colors baked from a
  theme JSON, which fights our CSS-variable token model and the streaming
  re-parse) and over **highlight.js** (comparable, but its auto-language
  detection is wasted here — LLM fences are labelled, e.g. ```rust, so we always
  know the language, and Prism's per-language explicit imports give tighter
  bundle control).
- **`DOMPurify`** to sanitize before `{@html}`. The user has stated security is
  not a goal here, but DOMPurify is small, synchronous, and adds no meaningful
  latency, so we keep it for free correctness rather than spend any effort
  hardening beyond the default profile.

### Documentation the implementing agent must read first

- marked usage & options: https://marked.js.org/using_advanced
- marked custom renderer / extensions: https://marked.js.org/using_pro
- marked-highlight: https://github.com/markedjs/marked-highlight
- Prism manual highlighting (`Prism.highlight`) & bundler usage: https://prismjs.com/docs/Prism.html and https://prismjs.com/#basic-usage
- DOMPurify config (`sanitize`, `ADD_ATTR`, profiles): https://github.com/cure53/DOMPurify
- Svelte `{@html}`: https://svelte.dev/docs/svelte/@html
- Token model & "name a role not a hue": `docs/ui-conventions.md` and `src/app.css`

---

## Milestone 1 — Markdown parsing pipeline + `Markdown.svelte` primitive

### Goal & Outcome

A reusable `ui/` primitive that turns a string of (possibly partial) Markdown
into safe, highlighted HTML, with interactive code blocks — independent of where
it's used.

- Given Markdown text, renders formatted HTML: paragraphs, bold/italic, lists
  (ordered/unordered/nested), headings, blockquotes, links, tables, inline code,
  and fenced code blocks.
- Fenced code blocks are syntax-highlighted for the common languages we expect
  from these harnesses (at minimum: rust, typescript, javascript, tsx/jsx,
  html/markup, css, python, bash/shell, json, yaml, toml, sql, diff, markdown);
  an unknown or absent language falls back to unstyled monospace, never an error.
- Each fenced code block shows a language badge and a **Copy** button that copies
  the block's source (not the highlighted markup) and shows transient "Copied"
  feedback.
- Links open in the user's external browser; clicking a link **never** navigates
  the app's own webview away.
- Incomplete/streaming Markdown (unclosed fence, dangling emphasis) renders
  without throwing and without destroying surrounding DOM.

### Implementation Outline

Create a shared marked configuration module and the `Markdown.svelte` primitive
under `src/lib/components/ui/`.

- **Parsing contract.** A single configured `marked` instance: `gfm: true`,
  `breaks: true` (single newline → `<br>` — the right default for chat-style LLM
  output). Register `marked-highlight` with a **synchronous** highlight function.
  **Guard the grammar lookup explicitly:** check `Prism.languages[lang]` first and
  only call `Prism.highlight(code, Prism.languages[lang], lang)` when it's truthy;
  otherwise return the **HTML-escaped** raw code (monospace, no token spans).
  `Prism.highlight` has version-dependent behavior when the grammar is
  `undefined` (some builds throw), so the guard — not the existing DoD test
  passing by luck — is what guarantees unknown languages never break a stream.
  Parsing MUST stay synchronous end-to-end (no async marked extensions) — async
  parsing reintroduces the streaming race we rejected.
- **Prism languages are imported explicitly** (the languages listed above), not
  via Prism's CDN autoloader. This is an offline desktop (Tauri) app; the
  autoloader fetches grammars over the network and would silently fail.
- **The component.** Props: `text: string` and an optional `class`. A
  `$derived` computes `DOMPurify.sanitize(marked.parse(text))` and the body is a
  single `{@html …}` inside a container div carrying a stable class
  (e.g. `markdown-body`) that Milestone 2 styles. Because each text segment gets
  its own instance, this `$derived` is the structural memoization described
  above — do not add a manual cache.
- **Code-block chrome is part of the produced HTML, not injected after render.**
  Override marked's `code` renderer (or post-process the token) to wrap each
  fenced block in the badge + Copy-button markup. The Copy button is plain markup
  here; its behavior is wired by delegation below. **Do not stash the raw source
  in a `data-` attribute or a hidden sibling** — code routinely contains quotes,
  `<tags>`, `&`, newlines, JSON, and heredocs, which an attribute round-trip can
  corrupt or which a hidden node duplicates (bloating the DOM for large blocks).
  Copy reads from the rendered element instead (below). The Copy button must sit
  **outside** the `<code>` element so its own label isn't captured by
  `textContent`.
- **Sanitizer config.** `DOMPurify.sanitize` runs on the parsed HTML before
  `{@html}`. The default profile already preserves `class` on standard elements,
  which is all the highlighter and chrome need (Prism's `token …` spans + the
  header/badge/button classes) — but **state this explicitly in the code** so a
  later tightening of the config can't silently strip `class` and kill
  highlighting. No custom `ADD_ATTR` is needed once the data-attribute approach is
  dropped.
- **Interactions via event delegation on the container** (one set of listeners on
  the stable root, not per-block handlers in the HTML string, which the next
  re-render would discard):
  - **Copy:** click → find the enclosing code block → read **`<code>.textContent`**
    → `copyText` (`src/lib/native.ts`) → toggle transient "Copied" state on the
    clicked button. `textContent` reconstructs the original source exactly (Prism
    only wraps text in spans; entities decode back to literal characters), so this
    avoids the escaping/duplication failure modes of carrying the source
    separately. Because completed segments never re-render, the feedback state is
    stable for any block a user can realistically click; resetting via a timeout
    is sufficient.
  - **Links:** intercept anchor clicks → `preventDefault` → hand the `href` to a
    **dedicated, net-new Rust command** that opens it in the OS default browser.
    The existing `open_session_file` (`crates/app/src/lib.rs:286`) is **not**
    reusable — it runs `open -t <path>` (the `-t` forces a text editor and it
    takes a backend-resolved file path, not a URL). Add `open_external_url(url:
    String)` (macOS `open <url>`, no `-t`) plus a `capabilities/default.json`
    entry. **Validate the scheme in Rust before spawning:** forward only `http`
    and `https`; return `Err` for `file:`, `javascript:`, `data:`, relative, or
    unknown schemes (so a hallucinated `file://…` link can't open arbitrary local
    files). The frontend handler stays dumb — intercept, pass `href`, let the
    backend decide. (`mailto:` is intentionally excluded unless the maintainer
    wants email links live.)
- **Streaming-tail flicker — known tradeoff.** With highlighting inside the parse
  step, the actively-growing segment re-highlights every token, so a
  partially-typed token in the *live* code block may briefly change color as more
  characters arrive. This is confined to the single streaming block (completed
  blocks are memoized and stable) and Prism is fast enough that it's minor.
  **Record this in a code comment.** If testing shows it's objectionable, the
  documented fallback is: render the live segment's code as plain monospace and
  only highlight once the segment is finalized — do **not** build that special
  case pre-emptively.

### Definition of Done

- **Unit tests** on the parse/sanitize output (assert on the produced HTML
  string / rendered DOM):
  - Core constructs render: headings, nested lists, bold/italic, blockquote,
    table, inline code, and a fenced block that carries a language class and
    Prism token spans **that survive sanitization** (asserts the `class`-preserve
    config).
  - Unknown-language and no-language fences render as monospace without throwing
    (covers the explicit grammar guard).
  - Partial input — unclosed ``` fence and a dangling `[link` — parse without
    throwing and produce a sensible partial result.
  - A `<script>` / `onerror=` payload is neutralized by DOMPurify (free
    correctness check, even though security isn't a goal).
- **Component tests** for interactions:
  - Clicking Copy calls `copyText` with the block's exact source — **assert the
    content**, not just that it was called — using a payload that exercises
    `<tag>`, `&`, `"'`, a multiline JSON snippet, and a trailing newline.
  - Clicking a link calls the external-open path and does not navigate the
    webview.
- **Rust unit tests** on `open_external_url`'s scheme validation: `http`/`https`
  are forwarded; `file:`, `javascript:`, `data:`, relative, and unknown schemes
  return `Err` and spawn nothing.
- Documented known limitation: live-segment highlight flicker + the recorded
  fallback.

---

## Milestone 2 — Token-mapped Markdown & Prism theme

### Goal & Outcome

The rendered Markdown looks native to Switchboard and switches light/dark with
the rest of the app, with zero new hues introduced outside the token model.

- Prose elements (headings, lists, blockquotes, links, inline code, tables) are
  styled to match the existing transcript density and the semantic tokens.
- Fenced code blocks read as first-class panels consistent with the existing
  tool-call boxes in `turnBody`.
- Syntax-highlight colors are defined once and flip correctly between light and
  dark via the `.dark` override — no JS recolor, no flash.

### Implementation Outline

- Add a `.markdown-body { … }` block (and a Prism `token` theme) to
  `src/app.css`, scoped under the container class so it can't leak into the rest
  of the app. Style headings/lists/blockquotes/links/inline-code/tables/`pre`
  using **only** semantic tokens (`--fg`, `--muted`, `--panel`, `--raised`,
  `--border`, `--accent`, …) per `docs/ui-conventions.md` — **no raw hex.** The
  original proposal's prose CSS is a reasonable starting point structurally, but
  its hardcoded Prism palette (`#c084fc`, `#fb923c`, …) is explicitly **out** —
  map every `.token.*` rule to a semantic token instead.
- Prism's syntax categories (keyword, string, comment, function, number,
  punctuation, …) need readable, distinguishable colors in **both** themes. The
  current token set is small (one accent). If a faithful code theme needs a few
  more colors than the existing semantic roles provide, **introduce them as
  named tokens in `app.css` with light+dark mappings** (e.g. a small
  `--code-*` / `syntax-*` group) and document them — do not sprinkle one-off
  hues. Naming the role rather than the hue is the rule; this is the one place
  the plan expects new tokens, so call it out in `ui-conventions.md`.
- The code-block header (language badge + Copy button) reuses the existing
  surface/border tokens so it matches the tool-call box styling in `turnBody`.

### Definition of Done

- Visual verification in **both** light and dark (CSS isn't meaningfully
  unit-testable): a representative message with headings, lists, a table, inline
  code, links, and a highlighted multi-language code block reads cleanly and
  flips themes with no flash and no raw-hue leakage.
- Any new syntax-color tokens are defined with both mappings in `app.css` and
  noted in `docs/ui-conventions.md`.
- Code blocks visually align with the existing tool-call boxes (same border /
  surface family).

---

## Milestone 3 — Wire into the transcript

### Goal & Outcome

The primitive replaces raw-text rendering everywhere a message body appears, so
real conversations render formatted, with tool calls still interleaved and
streaming/auto-scroll behavior intact.

- Agent text segments render as Markdown; tool-call items continue to render
  between segments unchanged (`data-testid="turn-tool"` etc. preserved).
- User messages render as Markdown.
- The fan-out (multi-recipient) columns render Markdown identically to the
  single-column path.
- During streaming, formatting appears live as text generates; the transcript
  stays pinned to the bottom (and only re-parses the growing segment).
- A code block expanding height mid-stream doesn't break the auto-pin.

### Implementation Outline

- Replace the two raw-text renderers in `UnifiedTranscript.svelte` with
  `<Markdown text={…}>`: the agent text item inside the `turnBody` snippet and
  the user body inside the `userMessage` snippet. Because `turnBody` is shared by
  both the single-column path and the fan-out columns, the fan-out path is
  covered by the same change — verify, don't duplicate.
- Leave the tool-call item branch in `turnBody` exactly as-is; only the
  `item_kind === "text"` branch changes. The interleaving of text and tool calls
  is already handled by the `items` list — no data-model change.
- **Do not sub-branch on `item.kind` here.** This branch covers both
  `kind: "text"` (answer) and `kind: "thinking"` (reasoning), which today render
  identically; applying `<Markdown>` to the whole branch formats both and
  preserves that parity. The `text`/`thinking` split + the collapsible reasoning
  container is **M4.10's** work, which lands after this and will relocate the
  `thinking` case into its own container while still rendering the body through
  `<Markdown>` (see "Text vs. thinking items" above). Keeping the primitive
  content-agnostic is what makes that later split a clean addition rather than a
  rewrite.
- **Auto-scroll.** The `scrollSignal` `$derived` already sums `item.text.length`,
  so growing text still triggers the pin effect. Confirm the pin survives the
  height jump when a fence closes (plain lines become a taller bordered block).
  If a late layout/paint loses the pin, address it minimally (e.g. the effect
  already runs on `scrollSignal`; ensure it reads after the relevant DOM update)
  — do not redesign the scroll machinery.

### Definition of Done

- **Component tests** (per `AGENTS.md`: mock `invoke`/`listen`, drive realistic
  event sequences):
  - An agent turn whose text contains Markdown renders formatted, and a turn with
    `[text, tool_call, text]` still renders the tool-call box **between** the two
    formatted text segments.
  - A user message containing Markdown renders formatted.
  - A fan-out turn renders Markdown in each column.
  - Streaming: feeding a partial then completed text mutation updates only the
    affected rendering and does not throw on the intermediate partial.
  - Link click in a rendered message is intercepted (external-open path called,
    no webview navigation).
  - Auto-pin: appending content (including a block that grows in height) keeps the
    container scrolled to the bottom when already pinned.
- Existing transcript tests still pass (update any that assert on the old
  raw-text DOM shape — adjust assertions to the new rendered structure, do **not**
  delete coverage).
- No live/adapter tests apply — this milestone is frontend-only and touches no
  harness, adapter, or IPC contract.

---

## Out of scope

- Math/LaTeX rendering, Mermaid diagrams, image embedding.
- Collapsing/folding long code blocks or long messages.
- Per-block "open in editor" actions.
- Any change to how tool-call items are parsed or displayed.
- The collapsible/muted/labeled treatment that visually distinguishes
  *reasoning* (`kind: "thinking"`) from *answer* text — that is **M4.10**. This
  plan formats thinking prose with Markdown but does not subordinate it.
