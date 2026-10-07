# Show Claude Code's `ReportFindings` code reviews, and include them in copy and forward

Claude Code has a built-in tool, `ReportFindings`, that a model calls to deliver code-review findings as structured data instead of as reply text. Switchboard shows that call as a collapsed generic tool row. It also leaves the call out of every "what did the agent say" path: the copy button, forwarding to another agent, and workflow step output. So a review delivered this way is effectively invisible, and forwarding it sends nothing useful.

This plan makes Switchboard render a `ReportFindings` call as a visible findings card. It also treats the findings as part of the agent's answer, so copy, forward and workflows carry them as text.

## What happened, concretely

On 2026-10-07 the user asked the agent `coder-1` (project `switchboard-remote-control-plan`, working directory `~/repos/switchboard-remote-control-plan`) to review PR 119. The session file is `~/.claude/projects/-Users-shanekercheval-repos-switchboard-remote-control-plan/01a0ef82-374a-7bd0-9f1f-c2d2f56e36d5.jsonl` (Claude Code 2.1.289, model `claude-opus-5-5`).

1. `coder-1` ran Claude Code's built-in `/code-review` skill. The skill ran as a separate sub-session and returned seven findings as a JSON block in its text.
2. `coder-1` then called `ReportFindings` with all seven findings. The tool's only reply to the model was `7 findings reported.`
3. `coder-1`'s final text was "I found 7 issues, listed above. Fix the first two before merging…". Switchboard showed a collapsed wrench row named `ReportFindings`, so nothing was "above". Forwarding that turn would have sent only the final sentence.

## Decisions

These are settled. Carry the reasons into code comments and commit messages, because none of them can be recovered from the code alone.

- **Switchboard renders the call when it sees it, and does not set `CLAUDE_CODE_REPORT_FINDINGS`.** That environment variable is undocumented (found only in the CLI's code). When set, it makes `/code-review` run inside the agent's own conversation instead of a separate sub-session, which spends that agent's context. It also tells the model not to repeat the findings as text. Models call `ReportFindings` on their own without it (six calls across five local sessions, none with the variable set), so rendering is needed either way.
- **The card is display-only.** Claude Desktop's card has "Walk through in diff", "Apply fixes" and "Re-run review" buttons. They are not Claude Code features: two of them send ordinary chat messages and one drives Desktop's diff pane. The user can type the same request in the compose box, and forwarding already covers handing a review to another agent. Build no buttons other than copy.
- **A later outcome report renders as its own card.** A model can call `ReportFindings` again after fixing things, with each finding carrying an `outcome` (`fixed`, `skipped`, `no_change_needed`). Desktop matches those against the earlier card and updates its badges in place. Switchboard renders the second call as a separate card with outcome badges and never edits the earlier card. Matching findings across calls is fragile because the model can reword them. Without the environment variable, outcome reports only happen when a model decides on its own, so they are rare.
- **Findings count as answer text everywhere a turn's text is read.** That covers the copy button, manual forward, workflow output (`forward_from`, `last_output`, `responses_from`), forward's disk fallback, and forward readiness.
- **One Rust function turns findings into text, and the frontend uses that text.** The Rust facet carries the finished markdown (the `text` field, defined in milestone 1), and the copy button uses it. Copy and forward therefore produce the same findings text, and the conversion has no TypeScript twin to drift from it.
- **Findings text also feeds the transcript navigator's search and preview.** Searching for a file named in a finding finds the review. A review-only turn previews as "Code review · 7 findings": `previewLine` in `src/lib/markdown.ts` strips the `**`.
- **When the model also writes the findings as prose, copy and forward carry both.** Models often follow a `ReportFindings` call with prose covering the same issues. Copy and forward then include the review markdown and the prose, so a receiving agent reads each issue twice. The prose usually adds fix suggestions, reproduction steps and verification evidence. Detecting which paragraphs merely restate the findings would be guesswork, and a wrong guess deletes useful text. So nothing is de-duplicated.
- **Collapsed responses keep their normal height limit.** In a collapsed response the card is part of the preview, like answer text. It isn't folded into the "N tool calls" label, but it is subject to the existing 14rem preview cap (`PREVIEW_CAP_REM` in `UnifiedTranscript.svelte`). A tall card, or one after long prose, is partly hidden behind the usual fade and Expand control. Finding rows aren't expandable while the response is collapsed.
- **No Codex or Antigravity work.** Neither harness has an equivalent tool.

### Rejected alternatives

Keep these and their reasons so later review rounds don't reopen them.

- **Rendering cards outside the collapsed-response height limit,** so a review is never cut off. Rejected because a 32-finding card would make every collapsed response that contains a review arbitrarily tall, which defeats collapsing. Keeping the card in order while lifting it out of the cut-off area also means splitting a response into separately clipped pieces.
- **Leaving findings out of the "last answer block" copy mode** to avoid the prose repeat. Rejected because it brings back the original problem. For `coder-1`, that mode would copy only "I found 7 issues, listed above." Forward would still carry both anyway.
- **A collapse control on the card itself.** Not planned. The card is already one line per finding. The user will revisit the card's design after seeing it in the app.

## Read before starting

- `AGENTS.md`, especially "Testing", "Live testing against real harnesses" (live-test naming), and the rule against hand-rolled `cargo` commands.
- `crates/harness/src/facets.rs` (the `ToolFacet` contract) and `crates/harness/src/claude_code/facets.rs` (the Claude classifier).
- `crates/harness/src/forward.rs`, whose module comment explains why the live capture and the disk read must produce byte-identical text.
- `docs/harness-behavior.md` §3.6 (tool vocabularies and facet mapping).
- `docs/ui-conventions.md` (semantic color tokens and `src/lib/components/ui/` primitives).
- `docs/harness-update-review.md` §3 (the dependency surface list you will extend).
- The real call in the `coder-1` session file named above. Its `ReportFindings` record is line 284 and its tool result is line 285.

`ReportFindings` has no public documentation. Everything below about it comes from the installed CLI's code (`~/.local/share/claude/versions/2.1.289`) and from Claude Desktop's interface code. Desktop's interface is claude.ai's web code. The findings card lives in `https://assets-proxy.anthropic.com/claude-ai/v2/assets/v1/c06173196-nHUQvJLZ.js` (components `TQ` and `IQ`, parser `NQ`) and its outcome handling in `cc43287c9-D19YL2gT.js`. Both file names change with each Desktop release. You do not need to re-derive any of this. It is recorded here and goes into `docs/harness-behavior.md` in milestone 1.

## What the tool is

`ReportFindings` takes this input. The CLI validates it against a strict schema, so required fields are always present on a successful call:

| Field | Type | Notes |
| --- | --- | --- |
| `level` | optional string | One of `low`, `medium`, `high`, `xhigh`, `max`: the review's effort level. Absent in the `coder-1` call. |
| `findings` | array, at most 32 | Most severe first. Empty means the review found nothing. |
| `findings[].file` | string | Repo-relative path, e.g. `.gitignore`. Not absolute. |
| `findings[].line` | optional integer | 1-indexed line. |
| `findings[].summary` | string | One-sentence statement of the defect. |
| `findings[].short_summary` | optional string, at most 60 characters | "Compressed label for compact UI." |
| `findings[].failure_scenario` | string | Concrete inputs or state that lead to the wrong result. |
| `findings[].category` | optional string, at most 40 characters | Kebab-case slug, e.g. `correctness`, `efficiency`, `test-coverage`. |
| `findings[].verdict` | optional `CONFIRMED` or `PLAUSIBLE` | Set when a verification pass ran. |
| `findings[].outcome` | optional `fixed`, `skipped` or `no_change_needed` | Set only on a re-report after fixes. |

The tool's reply to the model is only `N findings reported.` or `No findings reported.`, so the `tool_result` text is useless for display. Read the `tool_use` input. The live stream and the session file carry the same `{name, input}` block, like every other Claude tool. `ReportFindings` can be a deferred tool (one the model loads through `ToolSearch` before calling), which changes nothing for parsing.

Whether the model also writes the findings as text varies. Five local sessions contain `ReportFindings` calls. In four of them the model wrote prose covering the same findings, usually adding fix suggestions and evidence. Two of those four calls were made inside subagents, whose output Switchboard never shows. In the `coder-1` session the model wrote nothing beyond "listed above". Never rely on the text.

## Milestone 1: the harness turns a `ReportFindings` call into a findings facet

### Goal and outcome

Give Switchboard a typed, tested representation of a review, built once in Rust, that every later milestone reads.

- A `ReportFindings` call in a live Claude stream arrives at the frontend as a tool event whose facet is `findings`. The facet carries the level, the parsed findings, and the finished markdown text.
- The same call read back from a session file (reopening the project) produces an identical facet.
- A call whose input doesn't have the expected shape still renders as today's generic tool row.
- `docs/harness-behavior.md` records how the tool behaves, so the next harness-update review checks it.

### Implementation outline

1. **Add a `Findings` variant to `ToolFacet`** in `crates/harness/src/facets.rs`, serialized with `facet_kind: "findings"`. This is an additive change. `ToolFacet` is `#[non_exhaustive]` and the frontend already defaults unknown `facet_kind`s to the generic row, so nothing breaks for older builds or the planned iOS client. Shape:

   ```rust
   Findings {
       level: Option<String>,
       findings: Vec<Finding>,
       /// The review as markdown: the single text form used by copy, forward
       /// and workflow output. Built once here so those paths cannot drift.
       text: String,
   }
   // Finding: file, line: Option<u32>, summary, short_summary: Option<String>,
   // failure_scenario, category: Option<String>,
   // verdict: Option<Verdict>, outcome: Option<FindingOutcome>
   ```

   `Verdict` serializes as the wire values `CONFIRMED` / `PLAUSIBLE`. `FindingOutcome` serializes as `fixed` / `skipped` / `no_change_needed`.

2. **Update the `ToolFacet` contract comment.** It currently promises "Paths are absolute" and "No line numbers". Both still hold for file-operation facets. A finding's `file` is a repo-relative display label carried verbatim, and its `line` is the model's claim, not a position Switchboard resolves. Scope those two contract notes to the facets they describe, and say why findings differ.

3. **Classify `ReportFindings` in `classify_claude_tool_facet`** (`crates/harness/src/claude_code/facets.rs`). Both the stream parser and the session-file parser call this one function, which keeps live and disk identical. Parse leniently, matching Desktop's parser:
   - `findings` missing or not an array: return `Other`.
   - A finding without string `file` and `summary` is dropped. A missing `failure_scenario` becomes empty.
   - `findings` non-empty but no finding survives: return `Other`, so a garbled call shows its raw input instead of an empty card.
   - `findings` empty: a valid facet with zero findings, meaning the review found nothing.
   - An unknown `verdict` or `outcome` value, or a non-positive or non-integer `line`, is treated as absent. It never drops the finding.
   - `level` is kept as given when it's a string.

4. **Build `text` with one function**, placed beside the classifier. This is the markdown that forward sends to another agent, so it must stand alone. It uses the full `summary` (never `short_summary`) and includes the failure scenario. Format (illustrative values, shortened):

   ```markdown
   **Code review · medium · 2 findings**

   1. `.gitignore:44` · correctness · Confirmed
      The ignore rules for the old paths were replaced rather than kept alongside the new ones, so files those rules hid now show up as untracked.
      Failure scenario: A developer who ran `make ios-crypto` before the move runs `git pull`, and `git status` lists their personal `Local.xcconfig`.
   2. `Makefile:110` · correctness · Plausible · Fixed
      `test`, `lint` and `test-browser` lack the new install prerequisite.
      Failure scenario: In a pre-move checkout, `make test` runs with no `desktop/node_modules`.
   ```

   Rules:
   - The header is `Code review · <level> · N finding(s)`, matching the CLI's own one-line label and Desktop's card. Without a level it's `Code review · N findings`. With zero findings it's `**Code review · no findings**` (plus the level when present) and has no list.
   - Each finding's first line is the location (`file:line`, or `file` alone), then whichever of category, verdict label (`Confirmed` / `Plausible`) and outcome label (`Fixed` / `Skipped` / `No change needed`) are present, joined by ` · `.
   - Indent every continuation line, including line breaks inside `summary` or `failure_scenario`, by the width of that item's marker: 3 spaces for `1. `, 4 for `10. `. A fixed 3 spaces breaks findings 10 and up. After a blank line inside a value, a 3-space line falls out of item 10, both in CommonMark and in Switchboard's own renderer (`marked` with `gfm: true, breaks: true`).

5. **Fixtures.** Record one paired capture (a stream file plus the session file the same run wrote) with the real CLI. Use a tiny prompt that makes the model call `ReportFindings` with two findings: one with every optional field, one with only the required fields. Record it the way `crates/harness/tests/fixtures/claude/tool-vocabulary*.jsonl` were recorded. Add a session-file fixture with a zero-findings call. Trim the `coder-1` session's two records (lines 284 and 285) into a third fixture: it's the shape a real `/code-review` produced, from a model that didn't repeat the findings as text.

6. **Live test.** Add `live_claude_report_findings_emits_findings_facet` (the `live_claude_` prefix is required by `make test-live-claude`). The prompt asks the model to load `ReportFindings` with `ToolSearch` if needed, call it once with one specified finding, then reply `ack`. Assert that a `ToolStarted` named `ReportFindings` arrives with a `Findings` facet whose finding has the specified file and line, and that the session file yields the same facet. If the model won't call the tool reliably even when told to, stop and report that to the user rather than loosening the assertion.

### Definition of done

- Unit tests on the classifier cover:
  - a full finding
  - a required-fields-only finding
  - zero findings
  - a missing `findings` key (expect `Other`)
  - all findings malformed (expect `Other`)
  - one malformed finding among valid ones (that finding dropped)
  - an unknown `verdict` or `outcome` (field absent, finding kept)
  - a `line` of `0` or `"12"` (treated as absent)
- Unit tests on the text builder cover:
  - the header with and without a level
  - singular "1 finding"
  - zero findings
  - a finding without a line
  - every badge combination order
  - a multi-line `failure_scenario` (indented continuation)
  - ten or more findings where a value contains a blank line, asserting 4-space indentation under `10. ` and checking that the app's `marked` settings keep the paragraph inside item 10
- Extend the existing live-versus-disk facet agreement test (`stream_and_session_file_facets_agree_per_tool_use_id` in `claude_code/facets.rs`) with the new paired fixture.
- `make test-live-claude` passes, including the new live test. Paste its output in the summary.
- `docs/harness-behavior.md` §3.6 gets a Claude paragraph on `ReportFindings`. It covers:
  - the input schema
  - that the tool result carries no findings
  - the `CLAUDE_CODE_REPORT_FINDINGS` gate and why Switchboard leaves it unset
  - that models call it without being asked
  - that the model may or may not also write the findings as prose, so copy and forward can carry both
  - the version it was verified against (2.1.289)
- `docs/harness-update-review.md` §3 gets a dependency-surface bullet: the `ReportFindings` name and input fields Switchboard parses.
- To check the real result, run the live test's prompt through the adapter, print the emitted event as JSON, and read the `facet` field. Then confirm the session-file load of the same session prints the same facet.

## Milestone 2: forward and workflows include the findings text

### Goal and outcome

Make the Rust "what did the agent say" paths carry the review.

- Forwarding a turn that contains a review sends the review's markdown to the receiving agent, in the position it had among the agent's text.
- A workflow step whose agent reported findings passes them to the next step through `forward_from`, `last_output` and `responses_from`.
- A turn that produced only a `ReportFindings` call (no text) counts as forwardable instead of failing with "has no forwardable text".
- The live capture (an agent that was mid-turn when the forward was submitted) and the disk read (an idle agent) still produce byte-identical text for the same turn.

### Implementation outline

Rust has two paths that compute a turn's text, and `crates/harness/src/forward.rs` documents that they must agree byte-for-byte:

- The **live capture** is used when the agent was mid-turn when the forward was submitted. It's built from events as they stream.
- The **disk read** (`latest_completed_agent_text`) is used when the agent was idle. It's built from the session file.

Both change together, and this milestone moves both rules into `forward.rs` so they sit side by side.

- **What counts.** A findings report counts as answer content only when its call is confirmed successful: `ToolCompleted { is_error: false }` live, and `is_error == Some(false)` on disk. Three cases contribute no text:
  - A failed call (`is_error: true`, e.g. a schema rejection, after which the model typically retries). Excluding it means a retry doesn't send the review twice.
  - A call with no result in a completed turn. The session-file parser sets `is_error: None` when a `tool_use` never got a `tool_result`.
  - A call stopped by a cancelled or failed turn.

  Milestone 3's frontend rule is the same, so all three paths agree.
- **A shared capture type in `forward.rs`.** Today the live rule is inline in the dispatcher loop: `captured_text.push_str` for `ContentChunk { kind: Text }`, `crates/dispatcher/src/lib.rs` around line 2269. Replace it with a small type in `forward.rs`, e.g. `TextCapture` with `observe(&AdapterEvent)` and `finish() -> String`. It does three things:
  - appends text chunks, as today
  - remembers each findings facet's `text` from `ToolStarted`, keyed by `tool_use_id`
  - when that call's `ToolCompleted` arrives with `is_error: false`, appends the text, prefixed with `\n\n` when the capture is non-empty

  Appending at completion rather than at start is what excludes failed and stopped calls. The result arrives before the next text block, so order is preserved. This type is chosen over testing the inline dispatcher code because it puts both halves of the byte-for-byte contract in one module, testable from the harness crate. `MockHarnessAdapter` can't replay arbitrary event sequences (it plays fixed `MockScenario` presets), and test-only presets would ship in the production crate.
- **Dispatcher.** Call `observe` at exactly the point where `push_str` runs today: after the checks that drop events once a turn has ended or been force-failed. Otherwise text arriving after the turn's end leaks into forwards. Call `finish` where the dispatcher currently takes `captured_text` at the terminal event. The dispatcher stays harness-agnostic: it only hands events to the capture type.
- **Disk path.** `concat_text_items` in `forward.rs` joins non-empty `Text` items with `\n\n`. Make it also emit the facet's `text` for each successful findings report, in item order, with the same `\n\n` rule. Rename the function and rewrite the module comment, which currently says tool output is excluded everywhere.
- **The separator after a report stays a parser change.** The Claude stream parser (`crates/harness/src/parser.rs`) adds the `\n\n` between text blocks itself (`pending_separator`). It does this only when earlier text was emitted, so the first text after a findings-only start would be glued to the findings. Make the parser count a successfully completed findings call as earlier answer content. That means remembering findings `tool_use_id`s from `ToolStarted`, and on a non-error result for one, setting the state that makes the next text block open with `\n\n`. The capture type doesn't add this separator. The replay test below proves the parser and the capture type work together.
- **No other change for workflows.** `last_output`, `responses_from` and `forward_from` all read the dispatcher's capture or the disk fallback, so they inherit the change.

### Definition of done

- Unit tests on the capture type cover:
  - text, then findings, then text
  - findings only
  - findings first, then text
  - a failed call followed by a successful retry, where the review appears once
  - a call that never completes
  - two reports in one turn
- Unit tests on the disk join cover the same cases, plus a findings call with `is_error: None` (excluded).
- A parser test asserts that the `\n\n` separator appears on the first text chunk after a completed findings call, and not after a failed one.
- A parity test in `crates/harness/tests/claude_adapter.rs` replays a stream recording through the real `ClaudeCodeAdapter` (via the `fake_claude` test binary) into the capture type. It asserts the result equals `latest_completed_agent_text` on the paired session file, byte-for-byte. The recording includes a turn where the review comes first and text follows. If milestone 1's capture lacks that shape, record one that has it.
- A dispatcher test shows that the dispatcher hands the capture type's output to waiting forwards and workflows. An existing `MockScenario` preset is enough, because the sequence cases are covered by the capture type's own tests.
- A test asserts that a findings-only turn passes `is_forwardable_text`.
- To check the real result, run two agents in a dev build (`make dev`). Have the first call `ReportFindings` (milestone 1's live-test prompt works) and forward its turn to the second. Read the forwarded user message in the second agent's transcript and confirm the review markdown sits inside the `=== START forwarded from … ===` block. Do it once with the first agent idle and once while it's mid-turn, which exercises both paths.

## Milestone 3: the transcript shows a findings card, and copy includes the review

### Goal and outcome

Make the review visible and copyable.

- A successful `ReportFindings` call renders as a findings card in the agent's response, outside the collapsed tool-call group.
- The card shows the header (`Code review · medium · 7 findings`) and one row per finding. Each row shows a badge, `file:line`, and the short summary (or the summary when there's no short summary). Expanding a row shows the full summary and the failure scenario.
- In a collapsed response (an older response's preview, or a recent response collapsed to its final answer block), the card is part of the preview like answer text. It isn't folded into the "N tool calls" label, and it's subject to the normal preview height limit. Its rows can't be expanded until the response is expanded.
- The card has a copy button that copies the facet's `text`. The turn's copy button includes the review in both copy modes.
- A failed or stopped `ReportFindings` call renders as today's generic tool row with its failed or cancelled status.
- Searching the transcript navigator for a file named in a finding finds the review.
- Reopening a project renders past reviews as cards, including the `coder-1` review once that project is opened in this build.

### Implementation outline

1. **Types.** Add the `findings` variant to `ToolFacet` in `src/lib/types.ts`, mirroring the Rust shape.

2. **One rule for "answer items".** Today `src/lib/state/unified.ts` decides what counts as the answer (`answerTextOf`, `lastAnswerTextOf`, `copyTextOf`), and its doc comment says every consumer must route through it. Keep that single place and extend it:
   - A findings report is a tool item with `facet_kind === "findings"` and `is_error === false`. This is milestone 2's rule: only a confirmed-successful call counts as answer content.
   - Full answer: every non-empty answer text item and every findings report, in turn order.
   - Last answer block: every findings report plus the last non-empty answer text item, in turn order. Findings are included because the last text block usually refers to them ("listed above").
   - Text for copy joins the selected items with `\n\n`, using `facet.text` for a findings report. Never rebuild the markdown in TypeScript.

   Expose the item selection itself, not only the joined string, because rendering needs the items (step 4).

3. **The card.** A new component renders the facet. `AgentMessageBody.svelte` renders it in place of `ToolCallWidget` when a findings call either succeeded (`is_error === false`) or is still waiting for its result while the turn is streaming. Every other findings call uses `ToolCallWidget`, which shows the failed or cancelled status. That covers a failed call, a call stopped by a cancelled or failed turn, and a reopened completed turn whose call never got a result. Every non-findings tool also keeps `ToolCallWidget`. With this rule the card is never shown for a call that copy and forward leave out.
   - The card renders as soon as `tool_started` arrives, because the input is complete at that point. Show no pending spinner on it.
   - Badges: an outcome badge (`Fixed`, `Skipped`, `No change needed`) when present, else a verdict badge (`Confirmed`, `Plausible`) when present. Show the category as a muted tag.
   - Pick colors from semantic tokens per `docs/ui-conventions.md`. Desktop uses danger for Confirmed, warning for Plausible, success for Fixed and neutral for the rest.
   - Use the existing copy-button primitive. A zero-findings card shows only the header.

4. **Collapsed responses.** `UnifiedTranscript.svelte` renders collapsed responses in two modes. `"answer"` (older responses) renders answer text items only. `"final"` (a recent response the user collapsed) renders `lastAnswerTextOf` as one markdown block. Make `"answer"` also render findings cards. Make `"final"` render the last-answer-block item selection from step 2: cards for findings, markdown for text.
   - Both modes keep the existing height clip (`PREVIEW_CAP_REM`). Don't lift cards out of it (see "Rejected alternatives").
   - In both modes the card renders its rows without the expand control, because expanding a row inside the clip would grow hidden content. Expanding the response gives the full card.
   - `turnHasHiddenDetail` and `hiddenItemsLabel` must not count findings reports as hidden tool calls, because they aren't hidden. One consequence: a tool call used to make a response's Expand control appear regardless of height, and a review-only response loses that. Its Expand control now depends only on the measured height (`clipOverflow`), which is why the definition of done includes a WebKit browser test.

5. **Rows and previews.** In `src/lib/toolRow.ts`, give the `findings` facet a verb (`Code review`), a detail (`medium · 7 findings`) and an icon. A failed call's generic row and the navigator's tool fallback preview then read sensibly. `agentProse` in `src/lib/transcriptIndex.ts` uses the full-answer selection from step 2, so findings text becomes searchable and previews a findings-only turn.

6. **Forward readiness.** `heldForwards.svelte.ts` checks `answerTextOf`, so a findings-only turn becomes "ready" with no extra change. This matches milestone 2's Rust rule.

### Definition of done

- Component tests (jsdom) for the card cover:
  - header text with and without a level
  - zero findings
  - a row using `short_summary` versus `summary`
  - row expand showing the failure scenario
  - each badge case
  - the card copy button copying `facet.text`
- `AgentMessageBody` tests cover which component renders, and what copy returns, for:
  - a findings call that succeeded (card, included)
  - a findings call still pending in a streaming turn (card, not yet included)
  - a failed call (generic row, excluded)
  - a call stopped by a cancelled turn and by a failed turn (generic row, excluded)
  - a reopened completed turn whose call has no result (generic row, excluded)
  - an unknown facet (generic row)
- Tests on the answer-item rule cover:
  - full and last-block modes with text, then findings, then text
  - findings only
  - a failed call excluded
  - two reports
- A navigator test asserts that a finding's file name matches in search, and that a review-only turn previews as "Code review · N findings".
- Following AGENTS.md's rule for components that wrap IPC and events, a `UnifiedTranscript` test drives `tool_started`, `tool_completed` and text events through the mocked listener. It asserts that:
  - the card appears
  - the card renders in both collapsed modes, without row-expand controls
  - the hidden-items label excludes it
  - the copy button's text includes the review
- A WebKit browser test (`tests/browser/`, using the existing mount helper) collapses a review-only response whose card is taller than the preview cap. It asserts the response's Expand control appears, and that expanding reveals every finding row.
- `make check` passes.
- To check the real result, run `make dev`. In a project with a Claude agent, give the milestone-1 live-test prompt, then confirm by looking:
  - The card appears with correct rows.
  - Expanding a row shows the failure scenario.
  - Collapsing the response keeps the card in the preview, with rows not expandable.
  - Pasting from both copy buttons gives the expected markdown.
  - After restarting the app, the reopened transcript shows the same card.

  If you can't see the app window, ask the user to do this pass and report what they saw. Don't substitute test results for it.

## Known limitations

- Outcome reports never update the earlier card (decided above).
- When the model also writes the findings as prose, copy and forward carry both (decided above).
- In a collapsed response, a tall card, or one after long prose, is partly hidden until the response is expanded (decided above).
- A review that a subagent reports inside its own run isn't shown. Switchboard drops subagent records by design (`parse_line` skips records with a `parent_tool_use_id`). The parent sees only the subagent's final text.
- `file:line` is a label, not a link. Switchboard has no diff view to anchor it in.
- If Claude Code renames the tool or its fields, calls fall back to the generic row. Milestone 1's live test and the harness-update-review bullet are how that gets noticed.

## Delivery

All three milestones are one PR on the branch `claude-report-findings`. Don't commit until the user says to. Stop after each milestone with a summary of what changed and the real-result checks you ran, so the user can route it for review.
