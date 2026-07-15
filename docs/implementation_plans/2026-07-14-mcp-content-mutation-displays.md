# First-class MCP content mutation displays

**Status:** Implemented; final manual visual/reopen verification pending. M7 live validation found
one external Antigravity/MCP-schema incompatibility recorded below.
**Branch:** `tiddly-mcp-diffs`, based on `main` at `3de604a` (PR #64)

## Problem statement

Switchboard already identifies MCP tool calls and shows their server, tool name, raw input, output,
and lifecycle state. It does not understand the semantic content of an MCP mutation, so a note edit
such as `edit_content { old_str, new_str }` renders as generic JSON instead of the inline snippet
diff that an equivalent filesystem edit receives.

The two configured Tiddly MCP servers supplied the initial, live-probed schemas. Those schemas
expose enough stable input structure to improve five mutation tools without fetching additional
data:

| Tool | Required mutation input | Intended display |
| --- | --- | --- |
| `edit_content` | `id`, `type`, `old_str`, `new_str` | Inline snippet diff for a note or bookmark body |
| `create_note` | `title`; optional `content` | All-added note-body diff, or an explicit no-body creation summary |
| `create_bookmark` | `url`; optional title/description/tags | Structured created-record summary, not a text-body diff |
| `edit_prompt_content` | `name`, `old_str`, `new_str` | Inline snippet diff for a prompt template |
| `create_prompt` | `name`, `content` | All-added prompt-template diff |

Two related tools are deliberately not specialized:

- `update_item` can carry replacement content but does not carry the prior content.
- `update_prompt` has the same limitation.

Presenting either as a normal removed/added diff would invent a before-state Switchboard does not
have. They continue to render as ordinary MCP calls with raw input and output.

The tool schemas above were read via the servers' read-only `tools/list` operation on 2026-07-14.
No mutation tool was invoked. The notes/bookmarks server name and prompts server name are user-chosen
harness aliases and are not stable identifiers. Runtime recognition is deliberately
provider-neutral: these Tiddly schemas define the initial semantic allowlist, but Switchboard does
not claim to identify Tiddly as a provider. A structurally identical tool under another server alias
receives the same neutral note/bookmark/prompt display.

## Required reading — read before implementing

Project sources (read these before changing the contract):

- `AGENTS.md` — build/test commands, frontend conventions, fixture/live-test vocabulary, and the
  requirement that stream and session-file behavior stay aligned.
- `docs/ui-conventions.md` — semantic tokens and component conventions.
- `docs/system-design.md` §6 and §9 — MCP tools remain harness-owned; Switchboard renders what the
  harness records and does not proxy model-invoked MCP tools.
- `docs/harness-behavior.md` §3.6 — current per-harness tool envelopes and facet mappings. This plan
  updates that section.
- `docs/implementation_plans/2026-07-09-ui-improvements.md` M3–M4 — original `ToolFacet` and inline
  tool-diff contracts, including snippet-relative diffs, content caps, and lazy raw-input rendering.
- `src/lib/components/ToolCallWidget.svelte` module comment — the body-rendering and failure-display
  performance invariants are load-bearing.

External references:

- MCP Tools specification (tool discovery, JSON Schema inputs, results, and `isError`):
  <https://modelcontextprotocol.io/specification/2025-11-25/server/tools>
- MCP server concepts (tools as schema-defined model-controlled operations and activity-log UI):
  <https://modelcontextprotocol.io/docs/learn/server-concepts>
- `jsdiff` / `structuredPatch`, used by the existing snippet-diff synthesis:
  <https://github.com/kpdecker/jsdiff>

The implementer must use the schema table in this plan as the reviewed scope. Do not query private
MCP configuration or print authorization headers to reconstruct it. If a fresh schema probe becomes
necessary because an observed call no longer matches, use `tools/list` only, keep credentials out of
command output and fixtures, and stop for review if the mutation schemas materially changed.

## Shared decisions and boundaries

These decisions apply to every milestone and must not be re-decided independently in each harness
or renderer.

### Keep MCP provenance; do not masquerade as a file

The specialized operation remains `ToolFacet::Mcp`, including its observed server alias and tool
name. Extend that facet with an optional normalized mutation description. Do **not** convert these
calls into the existing `Edit` or `Write` variants and do not invent paths such as
`prompt:code-review`:

- `Edit` and `Write` have documented filesystem-path contracts.
- Remote MCP records cannot be opened in an editor or joined to the Git view.
- Reusing a filesystem facet would replace the useful `server · tool` heading with `Edit`/`Write`
  and make future file-specific affordances unsafe.

This rationale must survive in a doc-comment beside the MCP mutation contract. State the semantic
rule, not the history of this plan.

### One provider-neutral structural classifier

Add one shared classifier that receives an already-decoded `(server, tool, arguments)` triple and
returns an MCP facet with either a normalized mutation or `None`. Every harness calls it after
unwrapping its own envelope.

Recognition is based on the exact tool name plus the required argument types described below, not
on the server alias or endpoint. This is an explicit product boundary: aliases are
user-configurable, the tool names and fields directly express the neutral operation being rendered,
and Switchboard does not have a stable provider/profile identity on a tool call. A different server
exposing the same semantic schema receives the same useful display under its own real alias. Do not
call this classifier or its output Tiddly-specific in code. Missing, null, or wrong-typed required
fields degrade to a normal MCP facet; classification never fails the transcript and never changes
the call's lifecycle.

The rejected alternative is plumbing a configured endpoint/profile identity through every
harness's live and hydration paths solely to restrict this display to two services. That would be
required if the product wanted Tiddly-exclusive behavior, but it is not the chosen behavior here.
Do not add that configuration coupling during implementation.

The accepted mutation shapes share a bounded target label and independent target-truncation state:

- **Text edit:** before text, after text, and `content_truncated` for those two sides only.
- **Text creation:** created body text and `content_truncated` for that body only.
- **Record creation:** an ordered list of display fields and `fields_truncated` for those values
  only.

Nest these shapes under the MCP facet as a tagged, non-exhaustive Rust enum mirrored by a TypeScript
discriminated union. The absence of a mutation must stay wire-compatible with existing MCP facets;
omit the optional field when it is `None` rather than serializing repetitive `null` values.

Target labels have a dedicated collapsed-display cap of 240 Unicode scalar values and a separate
`target_truncated` flag. Append the visual ellipsis from that state; never put the uncapped target in
the collapsed DOM or a `title` attribute. Text bodies and record fields continue to use the existing
facet content cap: cap before/after independently for edits, the body for text creation, and each
included value for record creation. Only `content_truncated` may drive `DiffView`'s existing “Diff
truncated” notice; `fields_truncated` gets record-specific copy, and target truncation gets only the
ordinary label ellipsis. The complete target and values remain available through the raw `input`,
which retains its current lazy, display-capped behavior.

### Input-derived intent, not an authoritative post-write snapshot

The normalized mutation is derived entirely from the tool input available at tool start. The
probed tools' minimal success results identify the affected record but do not return its complete
post-write body.
Switchboard must not issue a follow-up `get_item` or `get_prompt_content` call:

- It would add network latency and a new failure path to transcript rendering.
- A later read could observe another writer's state rather than the state produced by this call.
- Switchboard is rendering a harness-owned tool call, not acting as another MCP client for it.

The inline view therefore shows the requested mutation. `edit_content` may normalize whitespace
while locating `old_str`, so its diff is a semantic snippet diff rather than a byte-authoritative
record diff. Preserve this reasoning in the shared classifier/module documentation and in
`docs/harness-behavior.md`.

### Exact tool mappings

- `edit_content`: require string `id`, string `old_str`, string `new_str`, and `type` exactly
  `"note"` or `"bookmark"`. Target: `<type> · <id>`.
- `edit_prompt_content`: require string `name`, `old_str`, and `new_str`. Target:
  `prompt · <name>`.
- `create_note`: require string `title`; accept string or null/absent `content`. Target:
  `note · <title>`. The body is the supplied content or empty. A present non-null `content` of any
  other type makes the mutation malformed and falls back to basic MCP rather than pretending the
  note has an empty body.
- `create_prompt`: require string `name` and string `content`. Target: `prompt · <name>`.
- `create_bookmark`: require string `url`; accept optional string/null title and description and an
  optional string array of tags. Target: `bookmark · <title>` when a non-empty title exists,
  otherwise `bookmark · <url>`. Display only present values, in the stable order Title, URL,
  Description, Tags; join tags for display without changing the raw input.

Do not recognize `update_item`, `update_prompt`, relationship tools, deletions, metadata-only edits,
or any tool not listed above. Adding them requires a separate product decision based on an honest
before/after contract.

### Lifecycle and failure behavior

The mutation may render while the tool is running, just like an inline file edit. Once a call is
failed or cancelled, suppress the attempted mutation and render the existing bounded status/error
output. A successful call keeps its mutation inline; its minimal server output remains available
only when expanded. Raw input remains available behind the existing reveal.

This reuses the current tool-row lifecycle policy. Do not add tool-specific success detection or
parse minimal output metadata to override the harness's `is_error` signal. MCP distinguishes
protocol errors from tool execution errors, but each harness already normalizes both into the tool
lifecycle Switchboard consumes; this work must not introduce a second error path.

### Scope and dependency policy

- No new package or crate dependency is needed.
- Do not connect this transcript feature to Switchboard's separate MCP prompt-provider subsystem.
  The latter resolves user-invoked prompts before dispatch; these are model-invoked harness tools.
- Do not add remote-record navigation, record fetching, approval UI, or schema discovery at runtime.
- Raw tool names and inputs remain untouched for provenance and forward compatibility.
- Non-obvious rules above must survive into relevant module/doc comments. Code comments must explain
  the rule directly and must not reference this plan or milestone numbers.

---

## Milestone 1 — Normalized MCP mutation contract and classifier

### Goal & Outcome

Establish the one semantic contract every harness and frontend consumer will share.

- A decoded allowlisted edit or creation input produces a bounded, typed mutation description.
- The same tool under any non-empty server alias classifies identically apart from its displayed
  alias.
- Unknown tools and malformed known tools remain ordinary MCP calls rather than disappearing or
  becoming generic non-MCP tools.
- No classifier performs I/O or inspects harness configuration.

### Implementation Outline

Extend the normalized facet model in the harness crate with the optional mutation union described
under Shared decisions. Put the provider-neutral classifier beside that contract so the rules
exist once and are available to all four adapters.

The classifier must always preserve the supplied server and tool identity. Its fallback result is
`Mcp { server, tool, mutation: None }`, including when a known tool has malformed arguments. This is
different from builtin facet classifiers, which often fall back to `Other`: MCP identity is already
known and remains useful even when semantic enrichment fails.

Build target labels and the ordered bookmark field list in this shared layer. That prevents four
harnesses from developing different labels or accepting different partial schemas. Use exact field
types; do not coerce numbers, arrays, or objects into display strings. Optional bookmark fields may
be absent or null; if present with the wrong type, omit that display field rather than discarding an
otherwise valid bookmark creation. `create_note.content` is different because it is the body being
rendered: a present non-null wrong type discards the mutation enrichment. Required-field type
failures also discard only the mutation enrichment.

The Rust enum remains non-exhaustive, and the TypeScript mirror added later must default unknown
mutation discriminants to no specialized body. This preserves the project's additive wire-evolution
rule.

### Definition of Done

- Rust unit tests cover all five accepted tools with representative inputs and exact target labels.
- Alias-independence test: two different server names produce the same mutation payload while
  preserving their respective aliases; use one neutral non-Tiddly alias to pin that this is semantic
  recognition rather than provider identification.
- Target labels are capped at exactly 240 Unicode scalar values, set `target_truncated` independently,
  and do not set content/field truncation when only the target was shortened. Include a multibyte
  boundary case.
- Text edits cap both sides on UTF-8 boundaries; text creation and record fields are bounded; every
  path sets `content_truncated` or `fields_truncated` accurately without conflating either with
  `target_truncated`.
- Required-field edge cases: missing, null, wrong type, and invalid `edit_content.type` all produce a
  basic MCP facet with no mutation.
- Optional bookmark fields: absent/null fields are omitted, wrong-typed optional fields are omitted,
  tags must be a string array, title fallback to URL works, and output field order is stable.
- Empty create-note content and empty create-prompt content remain valid text creations; the frontend
  decides how an empty body is presented.
- `update_item`, `update_prompt`, and an unrelated same-server tool are explicitly tested as basic
  MCP facets.
- Serialization tests pin the tagged mutation wire shape and verify `mutation` is omitted when
  absent.
- Module documentation records why MCP mutations do not reuse filesystem facets, why matching is
  provider-neutral and alias-independent, and why the input is the only mutation source.

---

## Milestone 2 — Claude MCP envelope integration

This is intentionally small: Claude's live and disk paths already share one classifier over
byte-identical `{name, input}` blocks.

### Goal & Outcome

- Recognized content mutations from Claude render with the same enriched MCP facet live and after the
  project is reopened.
- All other Claude MCP tools retain their current label and generic body.

### Implementation Outline

After splitting Claude's `mcp__<server>__<tool>` name, route the decoded input through the shared MCP
classifier instead of constructing a bare MCP facet. Keep builtin classification unchanged. Because
the stream and session-file parser already call the same Claude classifier, do not add a second disk
mapping.

### Definition of Done

- Claude facet unit tests cover at least one text edit, one creation, a renamed server alias, and a
  malformed known tool falling back to basic MCP.
- A sanitized stream/session fixture pair for the same mutation yields identical facets, raw name,
  and raw input.
- Existing Claude MCP classification tests continue to prove an unrelated MCP call is still MCP.
- No credentials, real record IDs, or private note/prompt content enter fixtures.

---

## Milestone 3 — Codex live and hydration integration

### Goal & Outcome

- Codex content mutations carry the enriched facet when they arrive live.
- Reopening the same Codex session reconstructs the same mutation.
- Codex's late MCP identity correction cannot erase mutation enrichment or attach it to the wrong
  call.

### Implementation Outline

Use the shared classifier in the live `mcp_tool_call` start path, which already provides `server`,
`tool`, and object-valued `arguments`.

The session-file path needs the same classification after decoding the function call's JSON-string
arguments and namespace. Preserve its existing defensive support for records where the function call
lacks MCP identity and a later `mcp_tool_call_end` supplies the server/tool. When that late result is
paired, recompute the facet from the corrected identity **and the tool item's retained input** rather
than unconditionally overwriting it with a bare MCP facet. Sequencing is load-bearing: identity must
be corrected before classification, but lifecycle output/error/completion fields must still be
applied to the same `tool_use_id`.

Do not derive mutations from result content. Codex's result remains output/error evidence only.

### Definition of Done

- Live parser tests: recognized edit and creation starts contain the enriched facet; completion
  preserves it; a failed completion retains the facet in state but relies on lifecycle status for UI
  suppression.
- Session reconstruction tests cover a namespace-bearing MCP function call and a namespace-less call
  whose later `mcp_tool_call_end` supplies identity.
- A regression test proves late identity correction rebuilds/preserves the mutation rather than
  replacing it with `mutation: None`.
- Pairing tests include two adjacent MCP calls so a late result cannot enrich the wrong input.
- Live and hydrated representations of the same sanitized mutation have equal facets, names, inputs,
  outputs, and error status where those fields exist on both paths.
- Existing generic MCP and malformed-result tests remain green.

---

## Milestone 4 — Antigravity MCP wrapper normalization

### Evidence gate — complete before implementation

The wrapper below is grounded in local historical evidence, not in the checked-in fixture corpus:
five real `transcript.jsonl` files dated 2026-05-20 through 2026-05-29 contain 18 read-only MCP calls
with raw name `call_mcp_tool`, outer keys `ServerName` / `ToolName` / `Arguments`, string-valued
`Arguments`, and an object after one JSON decode. The calls cover both initial schema providers and
multiple read-only tools. This disproves the older `classify_tool_kind` comment's suggestion that the
transcript necessarily records only an unwrapped underlying name, but it does not prove that the
currently installed `agy 1.1.2` has kept the shape.

Before changing the parser, run one current-version Antigravity turn that invokes a harmless
read-only tool such as `get_context`. Inspect the resulting `transcript.jsonl` record and sanitize one
exact observed call into the repository fixture corpus, replacing server aliases, summaries, IDs,
and content while preserving record keys and JSON-string encoding. Record the CLI version and probe
date in fixture/module provenance. Never include authorization headers.

If the current record differs, stop and revise this milestone from the observed envelope before
writing classification code. Do not make a synthetic fixture conform to the historical shape. If it
matches, the outline below becomes the pinned implementation contract and the stale parser comment
must be replaced with the verified rule.

**Evidence recorded 2026-07-14:** a read-only `get_context` invocation on `agy 1.1.2` confirmed the
historical wrapper exactly. The sanitized wrapper record is pinned in
`tests/fixtures/antigravity/mcp-tool-wrapper.transcript.jsonl`; its surrounding result and terminal
records are synthetic so the fixture contains no private MCP content.

### Goal & Outcome

- Antigravity's generic `call_mcp_tool` transcript rows become correctly identified MCP calls with
  `server · tool` headings.
- Recognized content-mutation arguments receive the same normalized mutation facet as Claude and
  Codex, live and on reopen.
- Malformed wrappers continue to degrade safely without disturbing FIFO tool-result pairing.

### Implementation Outline

Antigravity records the MCP dispatcher rather than the underlying tool as the raw call:

- raw tool name: `call_mcp_tool`
- args: `ServerName`, `ToolName`, and `Arguments`
- every outer arg value in `transcript.jsonl` is itself a JSON-encoded string
- `Arguments` decodes to the underlying tool's JSON object

Add one Antigravity helper that recognizes exactly this wrapper, decodes the two identity strings and
the nested argument object, and returns the normalized MCP identity plus decoded arguments. Reuse the
adapter's existing one-level string-decoding rule; do not special-case the observed provider aliases.

Both the live transcript tail and session-file hydration consume the same recorded `tool_calls`
shape, so both must call the same helper/classifier path. Classify the wrapper as `ToolKind::Mcp` only
when server and tool are non-empty strings and `Arguments` decodes to an object. Otherwise retain the
current builtin/other fallback instead of fabricating partial identity.

Keep `ToolStarted.name` and `ToolStarted.input` raw (`call_mcp_tool` and its wrapper args) for
provenance. The MCP facet supplies the normalized heading and semantic body. Do not change the FIFO
start/result pairing or invalid-tool-call error handling.

The reason for preserving the raw wrapper while separately normalizing its identity must survive in
the Antigravity classifier's module documentation.

### Definition of Done

- The evidence gate produced a sanitized fixture from a current `agy 1.1.2` read-only invocation and
  documented its provenance; if the version changed before implementation, record the actually
  tested version instead.
- Unit tests decode a sanitized transcript-exact wrapper, including the extra JSON-string layer, and
  classify it as MCP.
- A generic read-only Tiddly wrapper such as `get_context` becomes a basic MCP facet with the correct
  server/tool heading; this proves wrapper normalization is not coupled only to mutation names.
- Each supported mutation family is represented across tests, with at least one edit and one create
  passing through the full wrapper decoder into the shared classifier.
- Malformed cases cover missing/empty server, missing/empty tool, invalid nested JSON, and nested JSON
  that is not an object; all degrade without panic or a fabricated MCP facet.
- Live-tail and session-file fixture tests produce identical facets from the same wrapper.
- Regression tests prove a normal result and an `invalid tool call` system error still complete the
  correct pending wrapper, including two adjacent wrapped calls.
- Fixtures contain synthetic server aliases, IDs, and content only; no authorization headers or
  private Tiddly data.

---

## Milestone 5 — Narrow Gemini MCP parity

This milestone is small and intentionally does not reopen Gemini builtin-facet work. Gemini remains
unprobeable with the current individual-account authentication failure documented in
`harness-behavior.md`.

### Goal & Outcome

- A Gemini MCP call using the already-supported `mcp__<server>__<tool>` envelope receives the same
  content-mutation enrichment in live and hydrated parsing.
- Non-MCP Gemini tools continue to use the generic facet exactly as today.

### Implementation Outline

For names that already classify as `ToolKind::Mcp`, split the known MCP name convention and call the
shared classifier with the existing parameters/args object in both Gemini parsers. Leave every
builtin path on `Other`; this is provider-neutral MCP enrichment based on the probed initial schema, not
a claim that Gemini's builtin vocabulary has been live-verified.

If the live and session-file Gemini records do not expose equivalent argument objects in the existing
fixtures, stop and document the mismatch rather than inventing a conversion.

### Definition of Done

- Fixture-driven live and session tests cover one recognized mutation and one unrelated MCP tool.
- Malformed MCP names and mutation arguments degrade to the existing generic/basic behavior without
  affecting tool-result pairing.
- `docs/harness-behavior.md` continues to state that Gemini behavior is fixture-supported but not
  live-probed.
- No Gemini builtin receives a new facet.

---

## Milestone 6 — First-class transcript rendering

### Goal & Outcome

Render the normalized mutations as concise, trustworthy inline content while preserving the current
MCP heading, lifecycle, and lazy-rendering guarantees.

- Allowlisted note/bookmark/prompt snippet edits show unified before/after diffs inline.
- New note and prompt bodies show as all-added content, like a newly written file but without
  pretending to have a filesystem path.
- New bookmarks show a structured created-record summary.
- Empty-body text creations remain legible instead of displaying an empty diff canvas.
- Failed or cancelled calls show their error/cancellation output and no attempted mutation.
- Expanding still reveals successful tool output and raw input.

### Implementation Outline

Mirror the Rust MCP mutation union in the frontend wire types. Unknown mutation discriminants must
fall back to the current basic MCP treatment; do not let a newer backend produce a blank body in an
older frontend.

Keep the existing MCP row vocabulary and plug icon. For a mutation-bearing MCP facet, use its target
as the muted row detail instead of previewing arbitrary raw JSON. Append an ellipsis when
`target_truncated` is true. The target arriving on the facet is already bounded; do not recover or
mount the full value in the collapsed row or its `title` attribute. The raw name and input remain in
the expanded provenance section.

Render text edits through `DiffView` using the existing snippet-diff synthesis and compact unified
style. Render text creations through the existing all-added synthesis. Note content and prompt
templates should use Markdown highlighting; Jinja constructs may remain plain tokens inside that
grammar rather than adding a new highlighter or dependency. The target is a display heading only,
not a `FileDiff.path` with file actions. When adapting a text mutation to `FileDiff`, forward only
`content_truncated` to `FileDiff.truncated`; target truncation must never produce DiffView's content
warning.

Reuse the current inline preview contract: at most 25 lines while collapsed, the same fade and
"Show N more lines" affordance, and all captured facet content on expansion. Backend byte caps and
frontend render windowing remain in force. Do not eagerly format the raw input or successful output.

For a valid text creation with an empty body, show the target plus concise copy such as "Created
without body content" and do not mount an empty diff. For bookmark creation, render the ordered
label/value fields as a compact structured block with creation/addition styling. It must not be
labeled as a content diff because the server has no bookmark body in this tool input. When
`fields_truncated` is true, use record-specific copy such as "Bookmark details truncated" rather
than DiffView's file/content warning.

Treat mutation-bearing MCP facets as inline-content facets for body mounting, but gate their body on
the same `interrupted` state as file edits/writes. The existing bounded failure preview and expanded
error output remain the only body content for a failed/cancelled call.

Update the component's load-bearing module comment to include MCP mutation previews in the deliberate
inline-rendering exception and to state that they are capped and input-derived.

### Definition of Done

- Frontend type tests or compile-time fixtures cover every mutation discriminant and the optional
  absent case.
- Tool-row unit tests preserve `server · tool` and the plug icon for all mutation-bearing MCP calls;
  target detail replaces raw JSON preview only when a mutation exists.
- Component tests cover note/bookmark text edit, prompt edit, note creation, prompt creation, bookmark
  structured creation, and empty-body creation.
- Diff assertions cover additions, removals, no trailing newline, multiline templates, and the
  snippet-relative nature of edit content without asserting fake absolute line numbers.
- A mutation with a target longer than 240 Unicode scalar values mounts only the bounded target plus
  ellipsis, does not put the full value in a `title` attribute, and does not show a diff-truncated
  notice when its content is complete.
- A normal target with capped before/after/body content does show the existing diff-truncated notice;
  a capped bookmark field shows only record-specific truncation copy.
- Collapsed text mutations cap at 25 rendered lines and expansion reveals all captured lines; a large
  single input remains bounded by the backend facet cap and does not cause raw JSON to mount while
  collapsed.
- Failed and cancelled cases for both edit and creation suppress the mutation body, show the existing
  status/error treatment, and retain raw input on expansion.
- Successful calls do not show minimal server output while collapsed; expansion shows output and raw
  input without duplicating the mutation body.
- Unknown mutation discriminants and basic MCP facets degrade to the existing generic MCP body.
- Manual visual verification in light and dark mode covers a long edit, a new prompt, a new bookmark,
  running state, success, failure, and cancellation. Confirm the target and server/tool heading do
  not truncate each other beyond the row's existing responsive behavior.

---

## Milestone 6.5 — Lazy previews for large text edits

> Superseded by Milestone 6.6 after performance calibration and automated behavior testing showed
> that source-size and line-count gates hid useful previews for large but computationally simple
> edits. This section remains as the implementation record for the earlier decision.

### Goal & Outcome

Prevent collapsed transcript rows from computing a complete structured diff when they will render
only a short preview. This applies consistently to existing filesystem edits and the MCP text edits
that reuse the same synthesis path.

- Ordinary edits retain the current 25-line preview, fade, and exact "Show N more lines" affordance.
- Oversized edits mount without running full diff synthesis while collapsed.
- Expanding an oversized edit computes and renders all captured before/after content.
- File writes and MCP text creations keep their existing bounded all-added path; this milestone does
  not redesign creation rendering.

### Implementation Outline

Before synthesizing any collapsed diff, evaluate the complete tool row against two shared budgets:
32,768 JavaScript string code units and 500 combined logical lines across all before and after
strings. Count both dimensions without allocating normalized strings or line arrays and stop as soon
as either budget is exceeded. A filesystem edit contributes every edit pair from every file in the
row; an MCP text edit contributes its single before/after pair.

Both limits are load-bearing. The source-size limit is intentionally below the backend's 256
KiB-per-content cap, while the line limit protects against thousands of short, unrelated lines that
are small in bytes but expensive for the line-oriented diff algorithm. Local worst-case calibration
with the installed `diff` package showed unrelated 1,000-line inputs on each side taking roughly 98
ms and 2,000-line inputs on each side taking roughly 363 ms despite using only about 9.8K and 21.8K
combined characters, respectively. The 500-combined-line limit keeps that synchronous collapsed
work in the lower-cost range without timing-dependent production logic or tests.

When a collapsed edit is within budget, preserve the existing behavior exactly: synthesize the full
diff, trim its mounted representation to 25 lines, and report the exact number of hidden diff lines.
When either row budget is exceeded, defer every diff in the row. Do not call `structuredPatch`, do
not build an approximate prefix diff, and do not claim an exact hidden-line count. Preserve the
filesystem path headings so the affected files remain visible, but show one compact row-level button
such as "Large edit — expand to view full diff" in place of all collapsed diff canvases. Spending a
shared budget on only the first files would make the preview depend on file order and could imply
that omitted files had no requested changes; the all-or-nothing deferred state is explicit and
consistent. An MCP text edit uses the same deferred affordance beneath its existing target detail.

Expansion remains the explicit boundary for full work: after the user expands the row, synthesize
the complete diff from the already-capped facet content and render it through the current compact
unified `DiffView`. Do not add workers, asynchronous loading state, caching, new backend fields, or a
second diff representation. The backend cap bounds expanded work, and keeping the existing
synthesis path avoids broadening this milestone into a diff-engine redesign.

Apply the behavior at the shared inline-edit presentation boundary so filesystem and MCP edits
cannot drift. The row-level decision must occur before entering the filesystem facet's per-file
synthesis loop. Keep content-truncation semantics unchanged: only the facet's content truncation
flag drives DiffView's truncation notice, and deferring a collapsed preview must not be described as
content truncation.

The row-scoped source and line budgets and the reason for the deferred state are load-bearing
performance decisions. Preserve that rationale in the nearby helper or component comment so a
future refactor does not restore full collapsed diff synthesis merely to recover the exact
hidden-line count.

### Definition of Done

- Unit tests cover the source and combined-line budgets immediately below, at, and above each
  boundary, including allocation-free early exit and a filesystem edit containing multiple pairs.
- A multi-file filesystem row whose individual files are each under budget but whose aggregate
  source exceeds a row budget defers every diff while retaining all file headings.
- A many-short-line fixture fits under the source-size budget, exceeds the combined-line budget, and
  takes the deferred path.
- A normal collapsed filesystem edit and MCP text edit retain their current diff preview and exact
  "Show N more lines" behavior.
- An oversized collapsed filesystem edit and MCP text edit render the deferred affordance without a
  diff canvas; expanding each renders the complete captured diff.
- Tests pin the performance contract that either over-budget condition is selected before any full
  structured-diff synthesis, rather than computing one or more diffs and discarding them afterward.
- Failed and cancelled edits continue to suppress both inline and deferred requested-change bodies.
- File write and MCP text-creation behavior and tests remain unchanged.
- Targeted `toolDiff` and `ToolCallWidget` tests pass, followed by `make check`.
- Milestone 6's outstanding manual light/dark verification is performed after this change so it
  covers the final collapsed and expanded edit behavior.

---

## Milestone 6.6 — Complexity-based edit previews

### Goal & Outcome

Preserve previews for large edits that are cheap to compare while protecting the transcript from
the smaller, high-edit-distance inputs that actually make structured diff synthesis expensive.

- An exact diff that completes promptly renders normally: up to 25 rows in full, otherwise the
  existing 25-row preview with an exact hidden-line count.
- Only a comparison that exceeds the collapsed computation deadline shows "Complex edit — expand
  to prepare diff."
- Expanding that exceptional edit prepares the exact diff asynchronously so agent streams and user
  interaction remain responsive.
- File writes and MCP text creations retain their bounded all-added previews and never enter the
  structured comparison path.

### Implementation Outline

Remove the source-unit and combined-line gates introduced in Milestone 6.5. They are not reliable
proxies for Myers-diff cost: calibration with the installed `diff` package showed a 100K-character
creation completing in under 1 ms, a 5,000-line append in roughly 5 ms, 500 unrelated lines per
side in roughly 23–29 ms, 1,000 unrelated lines per side in roughly 114 ms, and 2,000 unrelated
lines per side in roughly 480 ms.

Attempt exact collapsed synthesis under one absolute 25 ms deadline per tool row, passing only the
remaining time to each jsdiff comparison and including hunk conversion in the same boundary. The
transcript is a chunked agent-output surface rather than a continuously animated canvas, so an
occasional one-to-two-frame task is an acceptable trade for retaining substantially more useful
previews. Files that complete before the deadline keep their exact full/25-row preview; once a file
times out or exhausts the row deadline, that file and all later files use the deferred state without
starting more synchronous comparisons. This row scope prevents a multi-file tool call from
multiplying an individually acceptable delay. MCP text edits contain one comparison and follow the
same deadline contract.

Expansion must not retry exceptional comparisons as unbounded synchronous tasks. Use jsdiff's
callback mode, which yields to the event loop, and show a compact preparation state until each exact
diff is ready. Queue a row's deferred files sequentially behind one absolute five-second deadline,
passing only the remaining time into each file and its edit pairs. Completed files render as they
finish; a file that reaches the ceiling and work that has not yet started use the explicit
unavailable state. This prevents expanding one multi-file row from launching many long-running
comparisons on the main thread at once. Give each queued job a cancellation signal so a streaming
prop update can remove a superseded version before it starts; an already-running comparison remains
bounded by the row deadline and its result guard. Do not add a worker, caching layer, new backend
fields, or an approximate prefix diff. A prefix could omit the actual changed region and would be
more misleading than the explicit deferred state.

Keep truncation semantics unchanged: only backend content truncation drives DiffView's "Diff
truncated" notice. A timed-out comparison is a presentation state, not missing source content. The
25 ms deadline and the reason for selecting actual computation over size heuristics are load-bearing
and must remain documented beside the helper.

### Definition of Done

- Unit tests prove abortable synthesis passes only the remaining portion of one 25 ms row deadline
  through to jsdiff, includes hunk conversion in that deadline, and does not start later file work
  after exhaustion.
- A 100K-character simple addition still renders an inline diff, proving source size alone no
  longer suppresses previews.
- Component tests cover all three states: a complete diff of at most 25 rows, a successful exact
  diff capped to a 25-row preview, and a timed-out comparison with the deferred affordance.
- Expanding deferred filesystem or MCP edits shows preparation, then exact asynchronously computed
  diffs; multi-file work starts sequentially, shares one five-second row deadline, and has an
  explicit unavailable state for timed-out and unstarted work.
- A multi-file tool call preserves previews completed before its collapsed deadline while deferring
  the difficult and subsequent files rather than multiplying synchronous render time.
- Component tests prove stale asynchronous results cannot replace newer props or update an
  unmounted component, and a superseded queued version never invokes jsdiff before the current
  version starts.
- Failed and cancelled edits continue to suppress requested-change content, and creation rendering
  remains unchanged.
- Targeted `toolDiff` and `ToolCallWidget` tests pass, followed by `make check`.
- Manual light/dark verification covers ordinary previews, a large simple addition, a deferred
  complex edit, its preparation state, and its expanded result.

---

## Milestone 6.7 — Provider edit-content fidelity

### Goal & Outcome

Use the complete edit content each provider already persists so deletion and large-edit rows render
truthfully instead of degrading to unavailable or escaped, clipped input.

- Codex deletions render their captured prior content as an all-removed diff and use the Delete verb.
- Antigravity prefers its complete transcript for live and reopened turns, preserving native tool
  arguments and full edit content.
- Older or incomplete Antigravity conversations still load from the compact transcript; clipped
  encoded strings render only their valid decoded prefix and carry an honest truncation marker.

### Implementation Outline

For Codex, consume the observed `patch_apply_end` delete shape's `content` field through the same
content cap as additions and modifications. Preserve the existing content-unavailable fallback when
an older record omits that field.

For Antigravity, prefer `transcript_full.jsonl` after it catches up and retain `transcript.jsonl` as
the compatibility fallback. The files are written independently but carry the same observed record
order: live tailing may switch compact → full only when full has at least reached the emitted cursor,
must preserve that cursor, and must never switch back. Hydration may choose full only when it has at
least as many complete records as compact. Keep their argument
encodings explicit: the full transcript stores native strings/objects, while the compact transcript
stores JSON literals inside strings. This distinction is load-bearing because native source content
may itself be valid JSON and must not be decoded twice. Compact values clipped with Antigravity's
`<truncated N bytes>` suffix may recover only a valid JSON-decodable prefix and must set the existing
facet truncation flag; never render escaped source or the vendor suffix as file content.

Do not infer deleted content from the filesystem, add provider-specific frontend behavior, or change
the normalized facet wire contract.

### Definition of Done

- Codex facet tests cover a delete with captured content and the older content-absent fallback.
- Tool-widget tests prove one-file and homogeneous multi-file deletions render Delete with paths
  only while collapsed (and no collapsed diff work); expansion renders removed rows with no pending,
  added, or redundant `(deleted)` markers.
- Antigravity tests cover a lagging full transcript, the one-way caught-up live handoff without
  missing or duplicate events, hydration preference by completeness, native arguments that look JSON-encoded,
  native MCP argument objects, compact clipped-string recovery, UTF-8/escape-boundary recovery, and
  facet truncation.
- Live and hydration paths select the same Antigravity transcript representation and classifier
  encoding.
- `docs/harness-behavior.md` records the observed provider shapes and removes Codex deletion from the
  unverified gap list.
- Targeted Rust/frontend tests pass, followed by `make check`.

---

## Milestone 7 — Operational documentation and final validation

### Goal & Outcome

Make the new behavior reviewable and durable without turning any private MCP configuration into a
project dependency.

- Developers can see exactly how each harness carries MCP identity and arguments through its live
  and persisted transcript representations.
- The shared classifier documents its five semantic input contracts and limitations beside the
  implementation, without presenting Switchboard policy as vendor harness behavior.
- The complete offline suite and relevant live harness checks pass without requiring a Tiddly account
  in CI.

### Implementation Outline

Update `docs/harness-behavior.md` §3.6 only with harness-owned behavior: the direct
Claude/Codex/Gemini MCP envelopes, Antigravity's `call_mcp_tool` wrapper, the live-versus-persisted
identity/argument shapes, and Gemini's fixture-only status. Do not put the five semantic mappings,
server-alias-independent matching policy, Tiddly matching behavior, or frontend presentation in
that document; none is a property of a harness.

Keep the shared semantic contract beside `classify_mcp_tool_facet` in `crates/harness/src/facets.rs`:
the five exact tool/input schemas, provider-neutral rationale, input-derived limitation, malformed
fallback, and exclusion of full-replacement tools that carry no before-state. The implementation
plan remains the chronological design record; the classifier documentation is the durable contract
that changes with the code.

Do not add this to the README: it is an enhanced rendering capability, not a user-facing harness
limitation or setup requirement. Do not add Tiddly endpoints, aliases, tokens, real IDs, or private
content to documentation or fixtures.

No new Tiddly-dependent live test should be added. Such a test would require a developer's private
MCP configuration and either mutate persistent data or depend on provider-specific failure behavior,
making `make test-live` non-portable. Fixture-driven integration tests are the correct regression
layer for the parser shapes. Existing per-harness live suites still run before merge to catch broader
CLI drift; they are not expected to invoke Tiddly. M4's one-time read-only evidence probe is the
explicit exception: it establishes and sanitizes a current Antigravity envelope but does not become
a persistent test dependency.

### Definition of Done

- `docs/harness-behavior.md` reflects each harness's MCP envelope and live/hydration behavior without
  documenting shared semantic or frontend policy as vendor behavior.
- `classify_mcp_tool_facet` documents the five recognized input contracts, provider-neutral scope,
  malformed fallback, and the no-before-state boundary for full replacements.
- All new fixtures are synthetic and a repository search confirms no bearer tokens or authorization
  headers were introduced.
- Targeted Rust classifier/parser/session tests pass while iterating.
- Targeted `ToolCallWidget`, tool-row, diff-synthesis, reducer, and unified-transcript frontend tests
  pass while iterating.
- `make fmt` completes, and any formatting changes it produces are reviewed as part of the branch.
- `make check` passes, including Rust tests, frontend jsdom tests, lint/type checks, formatting, and
  the WebKit browser suite.
- Before merge, run the relevant existing live suites according to `AGENTS.md`:
  `make test-live-claude`, `make test-live-codex`, and `make test-live-antigravity`. Record Gemini as
  not runnable under its documented authentication gap. A live-suite failure unrelated to MCP must
  still be diagnosed rather than ignored.
- Manual reopen verification against existing mutation history confirms the same successful
  mutation renders equivalently live and after project reload for every locally available harness;
  do not create persistent test notes/bookmarks/prompts solely for this check.
- No dependency manifests or lockfiles change.

### Validation record (2026-07-15)

- `make check` passed, including Rust, frontend jsdom, lint/type/format, and WebKit browser coverage.
- The Codex live suite passed in full.
- Claude Code 2.1.210 initially exposed a context-window key drift: assistant events report
  `claude-opus-4-8` while `modelUsage` keys the same model as `claude-opus-4-8[1m]`. The parser now
  prefers an exact key and accepts only that observed qualifier as a fallback while preserving the
  assistant model for reopen provenance. The complete Claude live suite passed 28/28 after the fix.
- Antigravity's auth, adapter, full-stack, resume, shell-facet, and invalid-tool live checks passed.
  Its successful file-read smoke test was blocked before invocation by the locally configured MCP
  schema: Antigravity rejected an empty value in `tag_match`'s enum. This is external server/config
  validation rather than an adapter parsing failure.
- Gemini remained unavailable under the documented individual-account authentication gap.
- Manual visual and reopen equivalence remain pending; they must be checked against the final dev
  build without creating persistent MCP records solely for validation.

## Milestone dependency order

1. M1 defines the only normalized contract and classifier.
2. M2–M5 integrate each harness independently against M1. They may be implemented sequentially, but
   none may invent a harness-local mutation shape.
3. M6 consumes the stable wire contract after the harness paths are covered.
4. M6.5 makes the shared filesystem/MCP edit preview lazy before final visual verification.
5. M6.6 replaces size heuristics with row-bounded complexity measurement and asynchronous expansion.
6. M6.7 preserves provider-native deletion and complete Antigravity edit content before final visual
   verification.
7. M7 documents the behavior actually implemented and runs final cross-cutting validation.

If implementation evidence contradicts a reviewed schema or envelope, stop at that milestone and
ask for direction. Do not broaden the classifier, fetch remote state, or add a new presentation type
to make an unexpected record fit.
