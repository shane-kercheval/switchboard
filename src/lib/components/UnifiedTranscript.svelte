<script lang="ts">
  import { untrack } from "svelte";
  import type { AgentRecord, Attachment, ConversationItem, ProjectId } from "$lib/types";
  import { HEARTBEAT_TIMEOUT_MS } from "$lib/types";
  import { cn, formatDuration } from "$lib/utils";
  import { convertFileSrc } from "@tauri-apps/api/core";
  import {
    ChevronRight,
    ChevronsDownUp,
    ChevronsUpDown,
    Columns2,
    CornerUpRight,
    MessagesSquare,
    Send,
    SquareSlash,
    Workflow,
  } from "@lucide/svelte";
  import {
    cancelSend,
    getTranscriptRevision,
    retryAgentHydration,
    runtimes,
    transcripts,
    type Turn,
  } from "$lib/state/index.svelte";
  import {
    answerTextOf,
    buildUnifiedRows,
    copyTextOf,
    groupRenderBlocks,
    INITIAL_WINDOW,
    lastAnswerTextOf,
    REVEAL_BATCH,
    type RenderBlock,
    type UnifiedRow,
  } from "$lib/state/unified";
  import {
    FORWARD_SENTINEL,
    forwardCaptionFor,
    heldForwardsFor,
    type HeldForward,
  } from "$lib/state/heldForwards.svelte";
  import { cancelForward, openExternalUrl } from "$lib/api";
  import { agentCopy } from "$lib/agentCopy.svelte";
  import { shortcut } from "$lib/platform";
  import { HARNESS_COLOR } from "$lib/harnessDisplay";
  import Badge from "$lib/components/ui/Badge.svelte";
  import Disclosure from "$lib/components/ui/Disclosure.svelte";
  import HarnessIcon from "$lib/components/ui/HarnessIcon.svelte";
  import Markdown from "$lib/components/ui/Markdown.svelte";
  import CopyButton from "$lib/components/ui/CopyButton.svelte";
  import Spinner from "$lib/components/ui/Spinner.svelte";
  import StatusChip from "$lib/components/ui/StatusChip.svelte";
  import StopIcon from "$lib/components/ui/StopIcon.svelte";
  import ToolCallWidget from "$lib/components/ToolCallWidget.svelte";
  import ThinkingWidget from "$lib/components/ThinkingWidget.svelte";
  import ErrorDetailsDialog from "$lib/components/ui/ErrorDetailsDialog.svelte";
  import Button from "$lib/components/ui/Button.svelte";
  import Tooltip from "$lib/components/ui/Tooltip.svelte";
  import {
    isCompact,
    setManyOverrides,
    stateFor,
    toggleKey,
  } from "$lib/state/transcriptPreview.svelte";

  type AgentTurn = Extract<Turn, { role: "agent" }>;
  type NonUserRow = Exclude<UnifiedRow, { kind: "user" }>;

  /// `agents` is the active project's roster (for attribution + flattening
  /// their per-agent transcripts). `overlay` is the project's hydrated
  /// journal items (user messages + outcome markers). `loadStatus` drives the
  /// first-activation loading indicator and the project-load-failed state.
  let {
    projectId,
    agents,
    overlay = [],
    loadStatus = "complete",
    loadError,
    onRetryLoad,
    showOnboarding = false,
  }: {
    /// The active project. Compact-transcript state and per-unit overrides are
    /// read/written keyed by this id, so the component never reaches into the
    /// workspace selection itself.
    projectId: ProjectId;
    agents: AgentRecord[];
    overlay?: ConversationItem[];
    loadStatus?: "pending" | "loading" | "complete" | "failed";
    /// Verbatim error when `loadStatus === "failed"` (the whole-project
    /// conversation load rejected). Drives the Details affordance on the
    /// project-load-failed block.
    loadError?: string;
    /// Re-attempt the project conversation load. Supplied by the parent (which
    /// owns the project id). Absent → no Retry button rendered.
    onRetryLoad?: () => void;
    /// Render the full orientation block (how sends, recipients, fan-out, and
    /// Forward work) in the no-messages empty state instead of the one-line
    /// placeholder. The host enables this only for the un-split default view —
    /// one pane holding the whole roster — so the block appears exactly once
    /// per blank project, never repeated in every split pane.
    showOnboarding?: boolean;
  } = $props();

  /// Roster agents whose *own* history failed to load (the per-agent
  /// `hydration_error`), independent of the whole-project load. Surfaced as
  /// pinned transcript-region chrome — not interleaved turns — because a failed
  /// agent contributes no turns to anchor against, and "silently missing
  /// history" is the exact trust gap this surface closes.
  const failedAgents = $derived(agents.filter((a) => runtimes[a.id]?.hydration_error != null));

  /// Verbatim-error dialog state, shared by the project-load and per-agent
  /// failure affordances. The failure itself lives on agent/project state; this
  /// only holds which message is currently being shown.
  let detailsOpen = $state(false);
  let detailsTitle = $state("");
  let detailsMessage = $state("");
  let detailsText = $state("");

  function openDetails(title: string, message: string, details: string): void {
    detailsTitle = title;
    detailsMessage = message;
    detailsText = details;
    detailsOpen = true;
  }

  const agentById = $derived.by(() => {
    const map: Record<string, AgentRecord> = {};
    for (const a of agents) map[a.id] = a;
    return map;
  });

  const rows = $derived.by(() => {
    const turns: Turn[] = [];
    for (const agent of agents) {
      const slice = transcripts[agent.id] ?? [];
      for (const turn of slice) turns.push(turn);
    }
    // Filter to the live roster so a removed agent leaves no orphan column
    // (the journal overlay retains its original recipient set).
    const knownAgentIds = new Set(agents.map((a) => a.id));
    return buildUnifiedRows(turns, overlay, knownAgentIds);
  });

  /// Group the flat rows into render blocks: standalone rows, plus one block per
  /// fan-out whose responses lay out as per-recipient columns.
  const blocks = $derived.by(() =>
    groupRenderBlocks(
      rows,
      agents.map((a) => a.id),
    ),
  );

  /// Render-windowing: mount only a tail window of `blocks`, so opening a long
  /// transcript doesn't pay the full markdown-render cost up front. That cost is
  /// the bottleneck — building the rows/blocks is single-digit ms, but mounting
  /// every block and running its `renderMarkdown` (marked + Prism + DOMPurify per
  /// segment) is ~1s on a long history. Off-window blocks stay in memory but
  /// unmounted, so their Markdown never parses until they're revealed.
  ///
  /// The window is a top-cursor index, deliberately NOT a fixed-size tail: a tail
  /// count would unmount the oldest visible block on every append — re-parsing
  /// markdown and disturbing scroll for an unrelated new turn. The cursor only
  /// decreases (upward reveal) or holds, so appended turns always render and
  /// nothing already visible is unmounted. `INITIAL_WINDOW` is a tuning knob, not
  /// a contract; block count is a loose proxy for the real cost driver (render
  /// *items* — one agent block can hold dozens, a compact user row one), so it's
  /// sized generously to cover a viewport plus scroll buffer.
  ///
  /// Known limitation: select-all (and any future find-in-page) reaches only
  /// mounted blocks — unmounted history isn't in the DOM until scrolled to. A
  /// deliberate tradeoff for the load-time win. `INITIAL_WINDOW` / `REVEAL_BATCH`
  /// live in `$lib/state/unified` so the component and the browser-test bound
  /// share one definition.
  function blockKey(block: RenderBlock): string {
    return block.kind === "fanout" ? block.key : block.row.key;
  }

  /// Conversation identity: a change means the block list was restructured *under*
  /// the cursor and the window must re-pin to the tail. Three axes, each a
  /// distinct restructuring (a pure tail append changes none of them, so it must
  /// NOT reseed):
  /// - **project switch** — a wholly different conversation;
  /// - **visible-agent change** — a pane hides/shows an agent on the *same*
  ///   persistent component (keyed by pane id, not roster); without it a stale
  ///   high cursor slices the shorter array to empty. Order matters (it drives
  ///   row grouping / fan-out column order);
  /// - **oldest-block change** — a front insertion such as a late retry-hydration
  ///   (incl. the single-failed-agent case: empty at first `complete`, fills on
  ///   retry). The oldest key is stable across appends/streaming (both touch the
  ///   tail), so an ordinary new turn holds the cursor.
  /// NUL (`\u0000`) joins the identity axes: it can't occur in a UUID or a
  /// block key, so distinct axis values can't collide into one string. Spelled
  /// as an escape, not a raw byte, so the delimiter stays visible in source.
  const windowIdentity = $derived(
    [
      projectId,
      agents.map((a) => a.id).join(","),
      blocks.length > 0 ? blockKey(blocks[0]!) : "",
    ].join("\u0000"),
  );

  /// The window must be applied **in the derived**, not set from an `$effect`:
  /// an effect runs *after* the render it would correct, so the first paint with
  /// a freshly-loaded transcript would transiently mount — and markdown-parse —
  /// every block before the effect bounded it. So `firstVisibleIndex` derives a
  /// tail-bounded fallback for any not-yet-frozen identity, and the effect below
  /// only *freezes* that fallback into an absolute `cursor` (so later appends grow
  /// the window instead of sliding it, which would churn the oldest visible
  /// block). Until frozen — or while a stale cursor belongs to a since-changed
  /// identity — the fallback bounds the window, so the first render is never the
  /// whole transcript. The fallback also covers the loading phase (stale content
  /// from a re-open can't mount unbounded).
  let cursor = $state<number | null>(null);
  let frozenIdentity = $state<string | null>(null);
  const firstVisibleIndex = $derived(
    cursor !== null && windowIdentity === frozenIdentity
      ? cursor
      : Math.max(0, blocks.length - INITIAL_WINDOW),
  );
  const visibleBlocks = $derived(blocks.slice(firstVisibleIndex));

  $effect(() => {
    if (loadStatus === "complete" && windowIdentity !== frozenIdentity) {
      cursor = Math.max(0, blocks.length - INITIAL_WINDOW);
      frozenIdentity = windowIdentity;
    }
  });

  /// Held forwards whose recipients fall in *this* transcript's agents, paired
  /// with that pane-local recipient subset. A held "waiting…" row belongs to the
  /// pane(s) that will receive the forward — exactly like a queued send shows in
  /// its recipient's column — not in every pane (which is also why a recipient in
  /// another pane would resolve to "unknown" here). With a single pane this is
  /// the whole roster, so every held forward renders once.
  const heldForwardsHere = $derived.by(() => {
    const ids = new Set(agents.map((a) => a.id));
    return heldForwardsFor(projectId)
      .map((held) => ({ held, recipients: held.recipients.filter((r) => ids.has(r)) }))
      .filter((entry) => entry.recipients.length > 0);
  });

  /// The transcript recognizes a quoted block by matching the **canonical backend
  /// wire shape** — a synchronized cross-language contract with the Rust emitters
  /// (`crates/harness/src/forward.rs` `compose_forwarded_message` → `forwarded
  /// from`; `crates/workflow/src/template.rs` `aggregated_responses` → `response
  /// from`). Change a sentinel's wording on either side and both must change; these
  /// comments are a signpost, **not enforcement**. (Follow-up for durable
  /// enforcement: a shared fixture asserted by a Rust *and* a TS test, or structured
  /// provenance replacing string-sniffing — deferred; string-matching is the
  /// root-cause class of the B1 bug this fix addresses.)

  /// Fast single-line gate: does the body contain any quoted block? Anchored (`/m`),
  /// so no `/g` `lastIndex` state — used only to decide whether to take the banding
  /// path. Broader than `FORWARD_SENTINEL` (the manual-forward marker, in
  /// `heldForwards`): it also matches the `response from` aggregation shape.
  const QUOTED_BLOCK_SENTINEL = /^=== START (?:forwarded|response) from .+ ===$/m;

  /// One quoted block — `=== START (forwarded|response) from <agent> === … === END
  /// … ===` — covering both a forwarded block (manual or workflow `forward_from`)
  /// and a fan-in aggregation block (`aggregated_responses`). Captures the START
  /// sentinel, the keyword, the agent, the inner content, and the END sentinel
  /// separately so the sentinels can be styled while the content renders as
  /// Markdown. Matched non-greedily so adjacent blocks don't merge; blocks don't
  /// nest, so this is unambiguous. The END keyword and agent are backreferences to
  /// the START (`\2`, `\3`), so a block only bands when its sentinels pair on both
  /// — the canonical backend shapes always do; stray/pasted sentinel-looking text
  /// won't mis-band (the backreferences match the captured text literally, so no
  /// regex injection from agent names).
  const QUOTED_BLOCK =
    /(=== START (forwarded|response) from (.+?) ===)\n([\s\S]*?)\n(=== END \2 from \3 ===)/g;

  type QuotedSegment =
    | { kind: "text"; text: string }
    | { kind: "quote"; start: string; inner: string; end: string };

  /// Split a message body into ordered segments — the user's typed text (or a
  /// rendered prompt) as `text`, each quoted block (forwarded or aggregated) as
  /// `quote` — so the transcript can give *only the quoted portions* a style-only
  /// band, leaving the user's own text plain. Text is kept verbatim (sentinels
  /// included); only the blank-line separators between segments are trimmed.
  function splitQuotedSegments(text: string): QuotedSegment[] {
    const segments: QuotedSegment[] = [];
    let last = 0;
    for (const m of text.matchAll(QUOTED_BLOCK)) {
      const idx = m.index ?? 0;
      const between = text.slice(last, idx).replace(/^\n+|\n+$/g, "");
      if (between !== "") segments.push({ kind: "text", text: between });
      segments.push({ kind: "quote", start: m[1]!, inner: m[4]!, end: m[5]! });
      last = idx + m[0].length;
    }
    const tail = text.slice(last).replace(/^\n+|\n+$/g, "");
    if (tail !== "") segments.push({ kind: "text", text: tail });
    return segments;
  }

  /// Whether compact mode is on for the active project.
  const compactEnabled = $derived(stateFor(projectId).enabled);

  /// A still-live response: genuinely streaming, not yet closed. These use the
  /// live-streaming cap, never the completed-preview compaction. A streaming-on-disk
  /// turn an outcome marker has closed (a dangling/cancelled-mid turn) is *not*
  /// live — it's terminal and collapses like any other response.
  function isLiveStreaming(turn: AgentTurn): boolean {
    return turn.status === "streaming" && !hasOutcomeFor(turn);
  }

  /// Whether an agent response can be collapsed: any terminal turn that has
  /// content — complete, failed, cancelled, or a dangling streaming-on-disk turn
  /// closed by an outcome marker. Only genuinely-live streaming and empty turns
  /// are excluded, so every real response in the stream collapses uniformly.
  function isCollapsibleResponse(turn: AgentTurn): boolean {
    return !isLiveStreaming(turn) && turn.items.length > 0;
  }

  /// Whether a fan-out column is a collapsible terminal response (its latest
  /// state is terminal and it has rendered content).
  function isCollapsibleColumn(colRows: NonUserRow[]): boolean {
    const state = columnState(colRows);
    if (state === "queued" || state === "streaming") return false;
    return colRows.some((r) => r.kind === "agent" && r.turn.items.length > 0);
  }

  /// The render key for a turn — matches the key its render site uses, so
  /// `latestResponseKeys` membership lines up there. A turn whose send fans out
  /// renders as a column (`fanout:…`); otherwise a standalone row.
  function previewKeyForTurn(turn: AgentTurn): string {
    if (
      turn.send_id !== undefined &&
      blocks.some((b) => b.kind === "fanout" && b.send_id === turn.send_id)
    ) {
      return `fanout:${turn.send_id}:${turn.agent_id}`;
    }
    return `agent:${turn.turn_id}`;
  }

  /// Preview keys of each agent's most-recent collapsible response. When compact,
  /// these render expanded by default instead of using the height-clipped
  /// preview. Per agent and by recency (`ended_at ??
  /// started_at`), so an agent's latest reply keeps this treatment even when
  /// other agents' replies sit below it.
  const latestResponseKeys = $derived.by(() => {
    // eslint-disable-next-line svelte/prefer-svelte-reactivity
    const latestPerAgent = new Map<string, { at: string; key: string }>();
    for (const row of rows) {
      if (row.kind !== "agent" || !isCollapsibleResponse(row.turn)) continue;
      const turn = row.turn;
      const at = turn.ended_at ?? turn.started_at;
      const prev = latestPerAgent.get(turn.agent_id);
      if (prev === undefined || at.localeCompare(prev.at) > 0) {
        latestPerAgent.set(turn.agent_id, { at, key: previewKeyForTurn(turn) });
      }
    }
    // eslint-disable-next-line svelte/prefer-svelte-reactivity
    const keys = new Set<string>();
    for (const v of latestPerAgent.values()) keys.add(v.key);
    return keys;
  });

  // No `content-visibility` containment on transcript blocks: render-windowing
  // (above) bounds the mounted set, so the off-screen-layout cost containment
  // existed to cut is already small by default. Keeping it actively broke the
  // upward reveal — off-screen blocks sit at size *estimates*, which flip to real
  // heights mid-correction and shift the reading position ~a block. With real
  // heights the existing `reanchor` holds a top-prepend exactly, so windowing
  // owns mounted-set size and scroll-anchoring owns position stability, with no
  // estimate machinery between them. Residual: a user who reveals deep history
  // grows the mounted set, so a forced full relayout scales with it (~3 ms at ~50
  // mounted blocks, ~18 ms once ~300 are revealed — measured in WebKit when 50
  // was the default window; the initial window is now 20, so cold open sits well
  // below the ~50 point). Past there, the answer is a true
  // sliding-window/virtualization follow-up, not CSS containment estimates.

  /// Clip + bottom-fade for a height-clipped preview. Absolute stops (not
  /// percentages) so a short message never fades; the fade starts around the
  /// halfway mark so "there's more below" is unmistakable. The `-webkit-` mask is
  /// explicit because the app runs in WebKit (Tauri/macOS).
  const PREVIEW_CLIP =
    "max-h-[14rem] overflow-hidden [mask-image:linear-gradient(to_bottom,black_7rem,transparent_14rem)] [-webkit-mask-image:linear-gradient(to_bottom,black_7rem,transparent_14rem)]";

  /// Whether each clipped preview's content actually exceeds the cap, keyed by
  /// preview key — measured from the DOM. A toggle is only worth showing when
  /// collapsing differs from expanding; for a clipped preview that means the text
  /// overflows. jsdom has no layout (stays false there), so the data-derived
  /// hidden-content check below drives toggles in tests.
  ///
  /// **Entries are deliberately NOT deleted on `destroy`.** The measurer mounts
  /// only while clipped, so expanding unmounts it; keeping the `true` is what
  /// leaves the re-collapse toggle visible on the expanded message. Deleting it
  /// here would read back `undefined → false` and drop the toggle. Growth is one
  /// boolean per distinct message — negligible. One observer per clipped preview
  /// is fine pre-virtualization (observers ≈ on-screen messages); revisit with a
  /// shared observer if virtualization or very long transcripts land.
  let clipOverflow = $state<Record<string, boolean>>({});
  function measureClip(node: HTMLElement, key: string) {
    const ro = new ResizeObserver(() => {
      clipOverflow[key] = node.scrollHeight - node.clientHeight > 1;
    });
    ro.observe(node);
    return {
      destroy(): void {
        ro.disconnect();
      },
    };
  }

  function turnHasHiddenDetail(turn: AgentTurn): boolean {
    return turn.items.some(
      (i) => i.item_kind === "tool" || (i.item_kind === "text" && i.kind === "thinking"),
    );
  }

  /// Whether expanding a response would reveal more than its collapsed view, so a
  /// toggle is meaningful. A clipped preview hides tool calls / reasoning (and
  /// clips overflowing text — the `clipOverflow` half); the latest-response view
  /// is expanded by default, so its toggle means "collapse to the final answer
  /// block."
  function responseHasMore(turn: AgentTurn, key: string, isLatestResponse: boolean): boolean {
    if (isLatestResponse)
      return turnHasHiddenDetail(turn) || answerTextOf(turn) !== lastAnswerTextOf(turn);
    if (turnHasHiddenDetail(turn)) return true;
    return clipOverflow[key] ?? false;
  }

  /// Summary line for a collapsed response's hidden detail, or null when nothing
  /// non-text is hidden. Surfaced above the collapsed body so it's clear that
  /// tool calls / reasoning are tucked away — a clip's fade only signals hidden
  /// *text*.
  function hiddenItemsLabel(turn: AgentTurn): string | null {
    const tools = turn.items.filter((i) => i.item_kind === "tool").length;
    const hasReasoning = turn.items.some((i) => i.item_kind === "text" && i.kind === "thinking");
    const parts: string[] = [];
    if (tools > 0) parts.push(`${tools} tool ${tools === 1 ? "call" : "calls"}`);
    if (hasReasoning) parts.push("reasoning");
    if (parts.length > 0) return parts.join(" · ");
    return null;
  }

  /// `hiddenItemsLabel` for a fan-out column — the first agent turn with hidden
  /// detail (a column is one `(send_id, agent_id)` turn in practice).
  function columnHiddenLabel(colRows: NonUserRow[]): string | null {
    for (const r of colRows) {
      if (r.kind === "agent") {
        const label = hiddenItemsLabel(r.turn);
        if (label !== null) return label;
      }
    }
    return null;
  }

  /// Shared footprint for the in-transcript meta-row icon buttons (preview
  /// toggle, fan-out toggle-all). Intentionally mirrors `CopyButton` — the
  /// control they sit beside — rather than `ICON_BUTTON_CLASS` (the app-chrome
  /// style, `hover:bg-raised`), so their hover state matches the copy button in
  /// the same row.
  const META_ICON_BUTTON =
    "text-muted hover:text-fg hover:bg-border/60 flex h-[26px] w-[26px] items-center justify-center rounded-full border border-transparent transition-colors";

  /// Send ids that are queued (dispatched, not yet started) across the project's
  /// agents — the per-agent `pending_sends` minus any being cancelled. A
  /// single-recipient queued send has a user message but no agent turn yet
  /// (turn_start removes the pending entry), so we render a "queued…" affordance
  /// under it — the single-send equivalent of a fan-out column's queued state.
  const queuedSendIds = $derived.by(() => {
    // eslint-disable-next-line svelte/prefer-svelte-reactivity
    const set = new Set<string>();
    for (const agent of agents) {
      for (const p of runtimes[agent.id]?.pending_sends ?? []) {
        if (!p.cancel_requested) set.add(p.send_id);
      }
    }
    return set;
  });

  /// `${agent_id}:${send_id}` keys of turns owned by a non-completed Outcome
  /// marker (cancelled/failed). The marker is the authority for a non-completed
  /// turn's status (the journal owns non-completed outcomes), so a standalone
  /// agent row whose turn is in this set must not render the live "Working…"
  /// footer (its harness status can read `streaming` for a cancelled-mid Claude
  /// turn) nor its own status chip (which would contradict the marker — e.g. a
  /// `failed`-on-disk Codex turn beside a `cancelled` marker). This is the
  /// single-recipient analog of the fan-out column's `colHasOutcome`; the
  /// backend stamps `send_id` on both the turn and its marker, so the join is
  /// exact. Turns with no `send_id` (pre-journaling/imported) are never in the
  /// set and render unchanged.
  const turnsWithOutcome = $derived.by(() => {
    // eslint-disable-next-line svelte/prefer-svelte-reactivity
    const set = new Set<string>();
    for (const row of rows) {
      if (row.kind === "outcome") set.add(`${row.agent_id}:${row.send_id}`);
    }
    return set;
  });

  function hasOutcomeFor(turn: AgentTurn): boolean {
    return turn.send_id !== undefined && turnsWithOutcome.has(`${turn.agent_id}:${turn.send_id}`);
  }

  function agentName(agentId: string): string {
    return agentById[agentId]?.name ?? "unknown";
  }

  function agentBorderColor(agentId: string): string {
    const harness = agentById[agentId]?.harness;
    return harness ? HARNESS_COLOR[harness] : "var(--border)";
  }

  /// A fan-out column's copyable prose: the joined answer text of its agent
  /// turns (reasoning + tool calls excluded — see `answerTextOf`).
  function columnText(colRows: NonUserRow[]): string {
    return colRows
      .filter((r) => r.kind === "agent")
      .map((r) => copyTextOf(r.turn, agentCopy.mode))
      .filter((t) => t.length > 0)
      .join("\n\n");
  }

  function fanoutText(columns: Extract<RenderBlock, { kind: "fanout" }>["columns"]): string {
    return columns
      .map((col) => {
        const text = columnText(col.rows);
        if (text.length === 0) return "";
        return `${agentName(col.agent_id)}:\n\n"""\n${text}\n"""`;
      })
      .filter((t) => t.length > 0)
      .join("\n\n");
  }

  /// A fan-out column's timestamp: its latest agent turn's start, or "" (queued).
  function columnAt(colRows: NonUserRow[]): string {
    for (let i = colRows.length - 1; i >= 0; i--) {
      const r = colRows[i]!;
      if (r.kind === "agent") return r.turn.started_at;
    }
    return "";
  }

  /// The model footer is shown only for COMPLETED turns: harnesses stamp
  /// placeholder models on terminal-failure events (Claude records the literal
  /// `<synthetic>`), so a failed/cancelled turn's model is a leaked sentinel,
  /// not information.
  function modelOf(turn: AgentTurn): string | undefined {
    return turn.status === "complete" ? turn.model : undefined;
  }

  /// A fan-out column's runtime selection footer: latest agent turn wins, matching
  /// `columnAt` and the copy aggregation's "column owns its agent rows" contract.
  function columnModel(colRows: NonUserRow[]): string | undefined {
    for (let i = colRows.length - 1; i >= 0; i--) {
      const r = colRows[i]!;
      if (r.kind === "agent") return modelOf(r.turn);
    }
    return undefined;
  }

  function columnEffort(colRows: NonUserRow[]): string | undefined {
    for (let i = colRows.length - 1; i >= 0; i--) {
      const r = colRows[i]!;
      if (r.kind === "agent") return r.turn.effort;
    }
    return undefined;
  }

  function formatTime(iso: string): string {
    const d = new Date(iso);
    if (Number.isNaN(d.getTime())) return "";
    return d.toLocaleString(undefined, {
      month: "short",
      day: "numeric",
      hour: "numeric",
      minute: "2-digit",
    });
  }

  /// Ticking clock for the live "quiet" counter. Updated once per second while
  /// the component is mounted; `now` is only read inside the quiet footer, so
  /// when nothing is quiet these ticks trigger no re-render.
  let now = $state(Date.now());
  $effect(() => {
    const id = setInterval(() => {
      now = Date.now();
    }, 1000);
    return () => clearInterval(id);
  });

  /// Elapsed silence for a quiet turn: the timer fired one full
  /// HEARTBEAT_TIMEOUT_MS after the last activity, so true silence is the time
  /// since `quiet_since` plus that threshold. Starts at one HEARTBEAT_TIMEOUT_MS
  /// when the indicator first appears and counts up.
  function quietElapsedMs(quietSince: string): number {
    return now - Date.parse(quietSince) + HEARTBEAT_TIMEOUT_MS;
  }

  /// A fan-out column's state, derived from its rows. A non-completed **outcome
  /// marker** (cancelled/failed, journal-sourced) is *authoritative* and outranks
  /// the harness-derived agent status: the journal is the source of truth for
  /// non-completed outcomes (`TurnStatus` has no `cancelled`), so a cancelled turn
  /// that the parser persisted as `streaming`/`failed` — or `complete` in the
  /// cancel-after-end race — still reads `cancelled`/`failed` here, not a live
  /// spinner or a mislabel. This is safe because a fan-out column is a single
  /// `(send_id, agent_id)` pair: the marker can only belong to *this* column's
  /// turn (if columns ever span sends, this must become send-scoped). Otherwise
  /// the agent turn's status, else "queued" (dispatched, no turn yet).
  /// "streaming"/"queued" are *live* — they keep cancel-send active.
  type ColumnState = "queued" | "streaming" | "complete" | "failed" | "cancelled";
  function columnState(colRows: NonUserRow[]): ColumnState {
    for (let i = colRows.length - 1; i >= 0; i--) {
      const r = colRows[i]!;
      if (r.kind === "outcome") return r.status;
    }
    for (let i = colRows.length - 1; i >= 0; i--) {
      const r = colRows[i]!;
      if (r.kind === "agent") return r.turn.status;
    }
    return "queued";
  }

  // Scroll behaviour, the way chat apps work: while the user is at the bottom the
  // view follows new content (streaming tokens, new sends); otherwise it holds
  // its position on *any* height change — a message collapsing/expanding, a
  // fan-out toggling, the live cap being removed when a turn completes — so
  // nothing jerks and whatever the user clicked stays put. We measure height
  // changes with a ResizeObserver and re-anchor ourselves because WebKit (the
  // Tauri webview) has no native CSS scroll-anchoring.
  let container = $state<HTMLDivElement | null>(null);
  let content = $state<HTMLDivElement | null>(null);
  let pinned = $state<boolean>(true);
  // The user's saved gap from the bottom, updated only by real scrolls. Holding
  // it constant across a resize keeps every element whose content-below is
  // unchanged (e.g. the toggle you just clicked) at the same place on screen.
  let distanceFromBottom = 0;
  // Content height at the last (re)anchor or scroll. A `scroll` event whose
  // height matches this is genuinely user-initiated (the content didn't change),
  // so it may unpin; one with a *different* height is the browser clamping
  // `scrollTop` as content changed (a message collapsing, the live cap dropping
  // on completion) and must NOT flip us off the bottom — otherwise the re-anchor
  // jumps to a stale position. Discriminating by content-change rather than by
  // input device is what lets scrollbar-drag and keyboard scrolling work too.
  let lastScrollHeight = 0;

  // The block at the top of the viewport (re-captured on every user scroll and
  // after every re-anchor pass), its offset from the viewport top, and its
  // height at capture time — what the user chose to read. While unpinned,
  // restoring this anchor is the only correction that keeps the reading
  // position still regardless of WHERE a height change happened: gap-from-bottom
  // maintenance shifts the view by exactly the change whenever it happens below
  // the viewport (a streaming response growing, a new turn arriving — the
  // read-while-streaming bug), and an upward reveal prepends a batch ABOVE it.
  // The gap is kept for two cases only:
  // - the change happened INSIDE the anchor block (its own height moved): the
  //   user expanded/collapsed the thing they're looking at, and the contract
  //   there is "the toggle I clicked stays put" — which gap-hold provides,
  //   since the toggle sits below the growth (footer-anchor test);
  // - the anchor target is unreachable: content below shrank past the clamp (a
  //   real collapse), where the contract is "hold the gap, don't slam into the
  //   bottom" (scroll-hold tests).
  let anchorEl: Element | null = null;
  let anchorOffset = 0;
  let anchorHeight = 0;

  function captureAnchor(): void {
    anchorEl = null;
    if (!container || content === null) return;
    const viewportTop = container.getBoundingClientRect().top;
    // Blocks are in document order, so the first one whose bottom edge crosses
    // the viewport top is binary-searchable.
    const kids = content.children;
    let lo = 0;
    let hi = kids.length - 1;
    while (lo <= hi) {
      const mid = (lo + hi) >> 1;
      if (kids[mid]!.getBoundingClientRect().bottom > viewportTop) {
        anchorEl = kids[mid]!;
        hi = mid - 1;
      } else {
        lo = mid + 1;
      }
    }
    if (anchorEl === null) {
      anchorOffset = 0;
      anchorHeight = 0;
      return;
    }
    const rect = anchorEl.getBoundingClientRect();
    anchorOffset = rect.top - viewportTop;
    anchorHeight = rect.height;
  }

  function onScroll(): void {
    if (!container) return;
    distanceFromBottom = container.scrollHeight - container.scrollTop - container.clientHeight;
    if (container.scrollHeight === lastScrollHeight) {
      pinned = distanceFromBottom < 32;
      captureAnchor();
    }
    lastScrollHeight = container.scrollHeight;
  }

  // Whether the previous reanchor pass saw conversation rows. Drives the
  // empty→first-rows transition below; not reactive state (only reanchor
  // reads/writes it).
  let hadRows = false;

  /// Pin to the bottom when the user is already there; otherwise keep what the
  /// user is reading still: anchor-restore when the change landed elsewhere,
  /// gap-hold when it landed inside the anchor block or past the clamp (see
  /// the anchor comment above). Advancing `lastScrollHeight` here means the
  /// `scroll` event our own `scrollTop` write triggers compares equal and is
  /// treated as user-initiated (it recomputes `pinned`/anchor from the
  /// position we just set — benign).
  function reanchor(): void {
    if (!container) return;
    // The empty state — the one-line placeholder or the onboarding block — is
    // a document, not a conversation: it reads top-down and has no "newest
    // content" to follow, so auto-scroll never touches it. Without this, a
    // taller-than-viewport onboarding block mounts scrolled to its tail.
    // `untrack` keeps the scrollSignal effect's dependency set unchanged.
    if (untrack(() => rows.length) === 0) {
      hadRows = false;
      return;
    }
    // First rows after the empty state: re-pin unconditionally, wherever the
    // user had scrolled within the block — the conversation starts at the
    // newest message and follows streaming, per the chat contract.
    if (!hadRows) {
      hadRows = true;
      pinned = true;
    }
    if (pinned) {
      container.scrollTop = container.scrollHeight;
      lastScrollHeight = container.scrollHeight;
      return;
    }
    const maxScroll = container.scrollHeight - container.clientHeight;
    let anchored = false;
    if (anchorEl?.isConnected === true) {
      const rect = anchorEl.getBoundingClientRect();
      const drift = rect.top - container.getBoundingClientRect().top - anchorOffset;
      const target = container.scrollTop + drift;
      if (rect.height === anchorHeight && target >= 0 && target <= maxScroll) {
        if (drift !== 0) container.scrollTop = target;
        // The gap genuinely changed (the height change landed elsewhere);
        // keep the stored gap honest so a later gap-hold corrects from
        // reality, not a stale value.
        distanceFromBottom = container.scrollHeight - container.scrollTop - container.clientHeight;
        anchored = true;
      }
    }
    if (!anchored) {
      container.scrollTop = maxScroll - distanceFromBottom;
    }
    lastScrollHeight = container.scrollHeight;
    // The just-settled position is the new reference: an expand that fell back
    // to gap-hold must not keep suppressing anchor-restore for the unrelated
    // changes that follow it (e.g. streaming resuming below).
    captureAnchor();
  }

  /// Content-change signal for the re-anchor effect. The store's transcript
  /// revision covers every produced-content write (it bumps in `setTranscript`
  /// — see its single-writer contract); `rows.length` covers overlay-driven
  /// structure (queued rows, journal outcome markers) that doesn't pass
  /// through the store writer. This replaces a full digest walk over every
  /// row's text/output lengths — O(transcript) of reactive-proxy reads per
  /// streamed chunk. String-joined so a simultaneous +1/−1 in the two parts
  /// can't alias to an unchanged sum. This reactive path also keeps
  /// follow-the-bottom testable under jsdom, where the ResizeObserver layout
  /// path is inert.
  const scrollSignal = $derived(`${getTranscriptRevision()}:${rows.length}`);

  $effect(() => {
    void scrollSignal;
    reanchor();
  });

  /// Height changes with no data change — a message collapsing/expanding, a
  /// fan-out toggling, the live cap dropping when a turn completes — re-anchor
  /// too. WebKit (the Tauri webview) has no native CSS scroll-anchoring, so the
  /// observer drives it for us.
  $effect(() => {
    const el = content;
    if (el === null) return;
    const ro = new ResizeObserver(reanchor);
    ro.observe(el);
    return () => ro.disconnect();
  });

  /// Upward reveal: a sentinel above the block list (rendered only while history
  /// is windowed off the top) is watched by an IntersectionObserver rooted on the
  /// scroll container; reaching it mounts the next older batch (`revealOlder`).
  /// The sentinel lives OUTSIDE `content`, so `captureAnchor` (which scans
  /// `content.children`) never anchors on it.
  ///
  /// A cursor decrement changes neither `rows.length` nor the transcript
  /// revision, so the `scrollSignal` re-anchor effect does NOT fire on a reveal —
  /// the ResizeObserver on `content` is the sole, correct trigger, and its
  /// `reanchor` restores the reading position as the prepend grows `content`.
  ///
  /// `revealing` latches one batch per scroll-to-top: it gates re-entry until the
  /// flush settles, and also drives the brief spinner — the reveal is synchronous
  /// (blocks are in memory) but mounting + parsing a batch's Markdown is real work.
  let revealSentinel = $state<HTMLElement | null>(null);
  let revealing = $state(false);
  let pendingReveal = false;

  function revealOlder(): void {
    if (firstVisibleIndex === 0) return;
    // A trigger that arrives mid-reveal is REMEMBERED, not dropped: the observer
    // only re-fires on an intersection *change*, so if the sentinel is still in
    // view when the latch releases it would otherwise never reveal again (a stuck
    // window). Coalesced to at most one pending batch.
    if (revealing) {
      pendingReveal = true;
      return;
    }
    revealing = true;
    // Decrement the absolute cursor (and pin the current identity, so the derived
    // uses the cursor rather than the tail fallback). `firstVisibleIndex` reflects
    // it on the next read.
    cursor = Math.max(0, firstVisibleIndex - REVEAL_BATCH);
    frozenIdentity = windowIdentity;
    // The reveal is synchronous (blocks are in memory). Growing `content` fires
    // the ResizeObserver → `reanchor`, which restores the reading position
    // (anchor-restore: the read block's own height is unchanged, only blocks
    // above it mounted). `revealing` gates re-entry to one batch per
    // scroll-to-top and drives the brief spinner; released after the frame.
    requestAnimationFrame(() => {
      revealing = false;
      if (pendingReveal) {
        pendingReveal = false;
        revealOlder();
      }
    });
  }

  $effect(() => {
    const node = revealSentinel;
    const root = container;
    if (node === null || root === null) return;
    // A top `rootMargin` pre-triggers slightly before the sentinel is fully in
    // view, so the older batch is ready as the user reaches the top.
    const io = new IntersectionObserver(
      (entries) => {
        if (entries.some((entry) => entry.isIntersecting)) revealOlder();
      },
      { root, rootMargin: "200px 0px 0px 0px" },
    );
    io.observe(node);
    return () => io.disconnect();
  });

  /// Top-edge fade for a capped live region, keyed by preview key: true once the
  /// region has scrolled down far enough that content sits above the visible top
  /// (so a mask cues "more above"). Stays off at the very top so the first line
  /// is never faded.
  let liveTopFade = $state<Record<string, boolean>>({});
  const LIVE_TOP_FADE =
    "[mask-image:linear-gradient(to_bottom,transparent_0,black_2.5rem)] [-webkit-mask-image:linear-gradient(to_bottom,transparent_0,black_2.5rem)]";

  /// Inner bottom-pin for a capped live region. Each streaming unit's scroll
  /// element gets its own instance (its own `pinned` closure), so columns pin
  /// independently. Starts pinned; stays pinned while the user is near the
  /// bottom (within the same 32px threshold as the outer transcript); releases
  /// when the user scrolls up and re-engages when they return. `args.signal` is
  /// the unit's streamed-content length — Svelte re-runs `update` when it
  /// changes, re-pinning to the newest activity if still pinned. `args.key`
  /// scopes the top-fade flag.
  function liveScroll(node: HTMLElement, args: { key: string; signal: number }) {
    let pinnedHere = true;
    const sync = (): void => {
      pinnedHere = node.scrollHeight - node.scrollTop - node.clientHeight < 32;
      liveTopFade[args.key] = node.scrollTop > 8;
    };
    node.addEventListener("scroll", sync);
    node.scrollTop = node.scrollHeight;
    liveTopFade[args.key] = node.scrollTop > 8;
    return {
      update(next: { key: string; signal: number }): void {
        if (pinnedHere) node.scrollTop = node.scrollHeight;
        liveTopFade[next.key] = node.scrollTop > 8;
      },
      destroy(): void {
        node.removeEventListener("scroll", sync);
        // A completed turn never re-streams, so its top-fade flag is dead now.
        delete liveTopFade[args.key];
      },
    };
  }

  /// A streaming unit's content-length signal, so `liveScroll`'s `update` fires
  /// on every streamed token/tool mutation (item counts can stay constant).
  function liveSignalOf(turn: AgentTurn): number {
    let n = turn.items.length;
    for (const item of turn.items) {
      if (item.item_kind === "text") n += item.text.length;
      else n += (item.output?.length ?? 0) + (item.completed_at !== undefined ? 1 : 0);
    }
    return n;
  }
</script>

{#snippet failureBanner(
  testid: string,
  message: string,
  onRetry: (() => void) | undefined,
  onDetails: () => void,
)}
  <div
    class="border-status-failed/40 bg-status-failed-soft/40 mb-3 flex items-center justify-between gap-3 rounded-md border px-3 py-2"
    data-testid={testid}
  >
    <span class="text-status-failed text-xs">{message}</span>
    <div class="flex shrink-0 items-center gap-2">
      {#if onRetry}
        <Button variant="secondary" size="sm" data-testid={`${testid}-retry`} onclick={onRetry}>
          Retry
        </Button>
      {/if}
      <Button variant="ghost" size="sm" data-testid={`${testid}-details`} onclick={onDetails}>
        Details
      </Button>
    </div>
  </div>
{/snippet}

<!-- `live` gates the streaming "Working…" footer (spinner + live cancel button).
     It defaults true for standalone rows; a fan-out column passes false when an
     authoritative non-completed Outcome marker has overridden the column to
     cancelled/failed, so a turn the parser persisted as `streaming` (a turn
     cancelled mid-flight) doesn't reopen with a phantom live affordance on a
     dead turn. -->
{#snippet turnItems(turn: AgentTurn)}
  {#each turn.items as item, i (i)}
    {#if item.item_kind === "text"}
      {#if item.kind === "thinking"}
        <ThinkingWidget text={item.text} />
      {:else}
        <Markdown text={item.text} />
      {/if}
    {:else}
      <ToolCallWidget tool={item} />
    {/if}
  {/each}
{/snippet}

<!-- `mode` selects how a response renders:
     - `"full"` — everything (text, reasoning, tool calls); the streaming `live`
       cap or the static completed view.
     - `"answer"` — answer prose only, for the height-clipped preview of an older
       response (tool calls + reasoning suppressed).
     - `"final"` — only the final answer prose block, for a latest response the
       user manually collapsed. The copy button remains independent and can
       still copy only the final answer block.
     The hidden-items indicator above the body (call site) signals what `"answer"`
     / `"final"` tuck away, so a tool-only response needs no in-body placeholder.
     `live` gates the streaming "Working…" footer (a fan-out column passes false
     when an outcome marker has closed a streaming-on-disk turn). -->
{#snippet turnBody(
  turn: AgentTurn,
  live: boolean = true,
  mode: "full" | "answer" | "final" = "full",
  liveCap: boolean = true,
)}
  {#if mode === "final"}
    {@const answer = lastAnswerTextOf(turn)}
    {#if answer.length > 0}
      <Markdown text={answer} />
    {/if}
  {:else if mode === "answer"}
    {#each turn.items as item, i (i)}
      {#if item.item_kind === "text" && item.kind === "text"}
        <Markdown text={item.text} />
      {/if}
    {/each}
  {:else if turn.status === "streaming" && live}
    <!-- Live cap: streamed content scrolls inside a bounded region (so several
         active agents can't each take over the transcript), bottom-pinned to the
         newest activity. A top mask fades content scrolled above the view; the
         working/cancel footer renders OUTSIDE the scroll region so it stays
         visible regardless of the inner scroll position. -->
    <!-- Cap at 3/4 of the transcript area (the `[container-type:size]` ancestor),
         so a long stream fills most of the view before it scrolls but never
         outgrows the area — `cqh` tracks the container, not the viewport, so a
         short window shrinks it too. -->
    {@const liveKey = previewKeyForTurn(turn)}
    <div
      class={cn(
        liveCap ? "max-h-[75cqh] overflow-y-auto" : "overflow-visible",
        liveCap && (liveTopFade[liveKey] ?? false) && LIVE_TOP_FADE,
      )}
      data-testid="turn-live-scroll"
      use:liveScroll={{ key: liveKey, signal: liveSignalOf(turn) }}
    >
      {@render turnItems(turn)}
    </div>
    {@render workingFooter(turn)}
  {:else}
    {@render turnItems(turn)}
    {#if turn.status === "failed" && turn.error}
      <div class="text-status-failed text-xs" data-testid="turn-error">{turn.error}</div>
    {/if}
  {/if}
{/snippet}

<!-- Disclosure shown above a collapsed body when tool calls / reasoning are
     hidden — the cue a fade can't give. Always visible (it's signalling, not
     chrome) and clickable to expand the whole response. -->
{#snippet hiddenItemsIndicator(key: string, label: string)}
  <button
    type="button"
    class="text-muted hover:text-fg inline-flex items-center gap-1 text-xs transition-colors"
    data-testid="hidden-items-indicator"
    aria-label={`Show ${label}`}
    onclick={() => toggleKey(projectId, key, compactEnabled)}
  >
    <ChevronRight class="h-3.5 w-3.5" aria-hidden="true" />
    <span>{label}</span>
  </button>
{/snippet}

{#snippet turnStatusLabel(status: AgentTurn["status"])}
  {#if status === "failed"}
    <StatusChip status="failed" />
  {:else if status === "cancelled"}
    <StatusChip status="cancelled" />
  {/if}
{/snippet}

<!-- A fan-out column's terminal status chips (cancelled/failed), from its agent
     turns' own status or an authoritative Outcome marker. Rendered LAST in the
     column body (after the indicator and content) so the collapsed and expanded
     views place the chip identically; an Outcome marker suppresses the turn's own
     chip. -->
{#snippet columnStatusChips(colRows: NonUserRow[], colHasOutcome: boolean)}
  {#each colRows as r (r.key)}
    {#if r.kind === "agent"}
      {#if !colHasOutcome}{@render turnStatusLabel(r.turn.status)}{/if}
    {:else if r.kind === "outcome"}
      {#if r.status === "cancelled"}
        <StatusChip status="cancelled" testid="outcome-cancelled" />
      {:else}
        <StatusChip status="failed" testid="outcome-failed" />
        {#if r.reason}<span class="text-muted text-xs"> — {r.reason}</span>{/if}
      {/if}
    {/if}
    <!-- system_marker rows never enter a fan-out column (no send_id), so they
         have no status chip here. -->
  {/each}
{/snippet}

{#snippet liveTurnControl(onclick: () => void, label: string, testid: string)}
  <button
    type="button"
    class="border-muted/40 text-muted hover:border-status-failed/60 hover:bg-status-failed-soft/70 hover:text-status-failed focus-visible:ring-accent focus-visible:border-status-failed/60 focus-visible:bg-status-failed-soft/70 focus-visible:text-status-failed inline-flex h-[26px] w-[26px] items-center justify-center rounded-full border-[0.5px] transition-colors focus-visible:ring-2 focus-visible:outline-none"
    data-testid={testid}
    aria-label={label}
    {onclick}
  >
    <StopIcon class="size-5" />
  </button>
{/snippet}

{#snippet workingFooter(turn: AgentTurn)}
  {@const rt = runtimes[turn.agent_id]}
  {@const quietSince =
    rt?.quiet_since !== undefined && rt?.in_flight_turn_id === turn.turn_id
      ? rt.quiet_since
      : undefined}
  <div
    class="mt-2 flex items-center gap-2 text-xs {quietSince !== undefined
      ? 'text-warning'
      : 'text-muted'}"
    data-testid="turn-working"
    data-quiet={quietSince !== undefined}
  >
    <!-- Until the turn has been silent past HEARTBEAT_TIMEOUT_MS it just shows
         "Working..." (no counter — the number would otherwise reset on every
         event). Once quiet, it's still alive on the backend, so this is a soft
         caution that counts up the silence — never a failure — and reverts to
         "Working..." the moment activity resumes. -->
    <span class="animate-pulse">
      {#if quietSince !== undefined}
        No response ({formatDuration(quietElapsedMs(quietSince))})...
      {:else}
        Working...
      {/if}
    </span>
    {#if turn.send_id !== undefined}
      {@const sendId = turn.send_id}
      {@render liveTurnControl(
        () => cancelSend(sendId, [turn.agent_id]),
        `Cancel turn for ${agentName(turn.agent_id)}`,
        "turn-live-control",
      )}
    {/if}
  </div>
{/snippet}

{#snippet queuedFooter(agentId: string, sendId: string, labelTestid: string, controlTestid: string)}
  <div class="text-muted mt-2 flex items-center gap-2 text-xs" data-testid={labelTestid}>
    <span class="animate-pulse">Queued...</span>
    {@render liveTurnControl(
      () => cancelSend(sendId, [agentId]),
      `Cancel queued send for ${agentName(agentId)}`,
      controlTestid,
    )}
  </div>
{/snippet}

{#snippet previewToggle(key: string, defaultCompact: boolean)}
  {@const compact = isCompact(projectId, key, defaultCompact)}
  {@const label = compact ? "Expand" : "Collapse"}
  <!-- Hover/focus-revealed only — the fade already signals a collapsed message,
       so an always-on control would just be noise. -->
  <button
    type="button"
    class="text-muted hover:text-fg hover:bg-border/60 inline-flex items-center gap-1 rounded-full border border-transparent px-2 py-0.5 text-xs opacity-0 transition-colors group-focus-within:opacity-100 group-hover:opacity-100"
    data-testid="turn-preview-toggle"
    aria-label={label}
    onclick={() => toggleKey(projectId, key, defaultCompact)}
  >
    <!-- Same expand/collapse glyph as the header control. -->
    {#if compact}
      <ChevronsUpDown class="h-3.5 w-3.5" aria-hidden="true" />
    {:else}
      <ChevronsDownUp class="h-3.5 w-3.5" aria-hidden="true" />
    {/if}
    <span>{label}</span>
  </button>
{/snippet}

<!-- Group control for a fan-out: collapses all its response columns when any is
     expanded, else expands them all. Writes per-column overrides (so each column
     can still be toggled individually afterwards) and only touches this group's
     columns. -->
{#snippet fanoutToggleAll(entries: { key: string; defaultCompact: boolean }[])}
  {@const anyExpanded = entries.some((e) => !isCompact(projectId, e.key, e.defaultCompact))}
  {@const keys = entries.map((e) => e.key)}
  {@const label = anyExpanded ? "Collapse all responses above" : "Expand all responses above"}
  <Tooltip {label} side="bottom">
    {#snippet trigger(props)}
      <button
        {...props}
        type="button"
        class={cn(
          META_ICON_BUTTON,
          "shrink-0 opacity-0 group-focus-within/responses:opacity-100 group-hover/responses:opacity-100",
        )}
        data-testid="fanout-preview-toggle-all"
        aria-label={label}
        onclick={() => setManyOverrides(projectId, keys, anyExpanded)}
      >
        {#if anyExpanded}
          <ChevronsDownUp class="h-4 w-4" aria-hidden="true" />
        {:else}
          <ChevronsUpDown class="h-4 w-4" aria-hidden="true" />
        {/if}
      </button>
    {/snippet}
  </Tooltip>
{/snippet}

{#snippet messageMeta({
  at,
  copyable = "",
  label = "",
  mt = "mt-1",
  spend = undefined,
  costUsd = undefined,
  model = undefined,
  effort = undefined,
  previewKey = undefined,
  previewDefaultCompact = false,
}: {
  at: string;
  copyable?: string;
  label?: string;
  mt?: string;
  spend?: AgentTurn["spend"];
  costUsd?: number | null;
  model?: string;
  effort?: string;
  previewKey?: string;
  previewDefaultCompact?: boolean;
})}
  <!-- Two zones on a flex row: cost/overage + expand/collapse toggle pinned LEFT, and
       the hover-revealed model/timestamp/copy pinned RIGHT (`ml-auto`). The gap
       between them collapses first as the row narrows; then the right cluster's
       text wraps (model over timestamp) and truncates with `…`. The toggle and
       copy button are `shrink-0` — never squished. -->
  <div class={`${mt} flex items-center gap-2`} data-testid="message-meta">
    <!-- Left: cost + overage marker, then per-message expand/collapse. The cost
         is always visible; the toggle is hover/focus-revealed (its own opacity),
         so overage stays anchored instead of shifting behind an invisible toggle.
         Two distinct cost gates (no `match harness`): the **cost** shows on
         `spend.real_spend` (the turn cost real money — for subscription Claude
         that's overage, since `total_cost_usd` is otherwise notional); the
         **"using credits" marker** shows on `spend.is_overage` specifically. They
         coincide for Claude, but a future pay-per-use harness would set
         `real_spend` without `is_overage` → cost shows, marker stays hidden. -->
    <div class="flex shrink-0 items-center gap-2">
      {#if spend?.is_overage}
        <span
          class="text-warning text-xs"
          data-testid="message-overage"
          title={spend?.overage_resets_at
            ? `Spending overage credits — window resets ${new Date(spend.overage_resets_at).toLocaleString()}`
            : "Spending overage credits"}>⚡ using credits</span
        >
      {/if}
      {#if spend?.real_spend && costUsd != null}
        <span class="text-muted text-xs" data-testid="message-cost">${costUsd.toFixed(4)}</span>
      {/if}
      {#if previewKey !== undefined}
        {@render previewToggle(previewKey, previewDefaultCompact)}
      {/if}
    </div>
    <!-- Right: per-turn model/effort (history — what this turn actually ran on),
         timestamp, and copy. Hover/focus-revealed. The text group wraps so a
         narrow column stacks model over timestamp, and each line truncates with
         `…` rather than squishing to more lines; the copy button sits OUTSIDE the
         wrap (`shrink-0`) so it is never squished. -->
    <div
      class="ml-auto flex min-w-0 items-center gap-2 opacity-0 group-focus-within:opacity-100 group-hover:opacity-100"
    >
      <div class="flex min-w-0 flex-wrap items-center justify-end gap-x-2">
        {#if model}
          <span class="text-muted max-w-full truncate text-xs" data-testid="message-model"
            >{model}</span
          >
        {/if}
        {#if effort}
          <span class="text-muted max-w-full truncate text-xs" data-testid="message-effort"
            >{effort}</span
          >
        {/if}
        {#if at}
          <time
            class="text-muted max-w-full truncate text-xs"
            datetime={at}
            title={at}
            data-testid="message-time">{formatTime(at)}</time
          >
        {/if}
      </div>
      {#if copyable}
        <span class="shrink-0">
          <CopyButton text={copyable} {label} testid="message-copy" />
        </span>
      {/if}
    </div>
  </div>
{/snippet}

{#snippet attachmentList(attachments: Attachment[])}
  <div class="mt-1.5 flex flex-wrap gap-1.5" data-testid="user-attachments">
    {#each attachments as attachment (attachment.path)}
      {#if attachment.kind === "image"}
        <!-- `convertFileSrc` turns the absolute staged path into an `asset://`
             URL the webview can load (a raw filesystem path can't be an <img
             src>); the asset protocol is enabled + scoped in tauri.conf.json. -->
        <img
          src={convertFileSrc(attachment.path)}
          alt={attachment.original_name}
          title={attachment.original_name}
          data-testid={`attachment-thumb-${attachment.label}`}
          class="border-border h-16 w-16 rounded-md border object-cover"
        />
      {:else}
        <span
          class="border-border bg-panel text-fg inline-flex max-w-[14rem] items-center gap-1.5 rounded-full border px-2 py-px text-xs"
          data-testid={`attachment-file-${attachment.label}`}
          data-kind={attachment.kind}
        >
          <svg
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            stroke-width="1.8"
            stroke-linecap="round"
            stroke-linejoin="round"
            class="text-muted h-3.5 w-3.5 shrink-0"
            aria-hidden="true"
          >
            <path d="M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8z" />
            <path d="M14 3v5h5" />
          </svg>
          <span class="truncate" title={attachment.original_name}>{attachment.original_name}</span>
        </span>
      {/if}
    {/each}
  </div>
{/snippet}

{#snippet userBody(row: Extract<UnifiedRow, { kind: "user" }>)}
  {#if QUOTED_BLOCK_SENTINEL.test(row.text)}
    <!-- Quoted blocks: give each forwarded (`forwarded from`) or aggregated
         (`response from`) block — and only it — a style-only band so the quoted
         agent output stands apart from the user's own typed text, which stays
         plain. Text is verbatim — the `=== … ===` sentinels are kept. The turn's
         `data-forwarded` marker stays forward-only (below); the band is purely
         presentational and covers both shapes. -->
    {#each splitQuotedSegments(row.text) as seg, i (i)}
      {#if seg.kind === "quote"}
        <!-- Border-only band: a neutral dark-gray left rule (the `muted` token —
             not the harness-colored agent rules, not the accent green). Extra
             `py` makes the rule extend past the text top/bottom, and `my` adds
             separation between adjacent forwarded blocks. The `=== … ===`
             sentinels render bold + monospace (verbatim, as plain text so the
             `===` isn't parsed as Markdown); the content between renders as
             Markdown. -->
        <div class="border-muted my-3 border-l-2 pl-3" data-testid="quoted-block">
          <div class="font-mono text-xs font-bold">{seg.start}</div>
          <Markdown text={seg.inner} />
          <div class="font-mono text-xs font-bold">{seg.end}</div>
        </div>
      {:else}
        <Markdown text={seg.text} />
      {/if}
    {/each}
  {:else}
    <Markdown text={row.text} />
  {/if}
  {#if row.attachments.length > 0}
    {@render attachmentList(row.attachments)}
  {/if}
{/snippet}

{#snippet userMessage(row: Extract<UnifiedRow, { kind: "user" }>)}
  {@const key = `user:${row.key}`}
  {@const defaultCompact = compactEnabled}
  {@const compact = isCompact(projectId, key, defaultCompact)}
  <!-- A user message has nothing hidden behind a collapse — only height — so it
       gets a toggle only when its text actually overflows the clip. -->
  {@const showToggle = clipOverflow[key] ?? false}
  <!-- Forward treatment: only the forwarded *blocks* get a style-only band (see
       `userBody`), so the user's own typed text isn't visually claimed as
       forwarded; the body renders VERBATIM (sentinels intact). `data-forwarded`
       (derived from the body's canonical sentinel, so durable across reload)
       marks the turn for tests/hooks. The partial-empty caption names which
       sources were included vs. skipped; it renders in the meta row (outside the
       collapse clip) so it stays visible even while collapsed — and is live-only
       (a skipped source leaves no trace in the body to rebuild it from). -->
  {@const forwarded = FORWARD_SENTINEL.test(row.text)}
  {@const caption =
    row.send_id !== undefined ? forwardCaptionFor(projectId, row.send_id) : undefined}
  <div class="group min-w-0 flex-1" data-testid="turn" data-role="user" data-forwarded={forwarded}>
    <div class="w-full max-w-full overflow-hidden rounded-xl bg-blue-100/20 px-4 py-2">
      <!-- Clip wraps the content inside the bubble (not the bubble itself). The
           clip + `measureClip` mount ONLY while compact (mirroring agent rows): on
           expand the measurer unmounts and the retained `clipOverflow[key]=true`
           keeps the re-collapse toggle alive, instead of the observer firing on
           the now-unclipped div and clearing it. -->
      {#if compact}
        <div class={PREVIEW_CLIP} use:measureClip={key} data-testid="preview-clip">
          {@render userBody(row)}
        </div>
      {:else}
        {@render userBody(row)}
      {/if}
    </div>
    {@render messageMeta({
      at: row.at,
      copyable: row.text,
      label: "Copy message",
      previewKey: showToggle ? key : undefined,
      previewDefaultCompact: defaultCompact,
    })}
    {#if caption !== undefined}
      <div class="text-muted mt-0.5 text-xs" data-testid="forward-caption">
        ↪ forwarded from {caption.included.join(", ")} · {caption.skipped.join(", ")} had no output
      </div>
    {/if}
  </div>
{/snippet}

{#snippet outcomeRow(row: Extract<UnifiedRow, { kind: "outcome" }>)}
  {@const harness = agentById[row.agent_id]?.harness}
  <div
    class="group space-y-1.5"
    data-testid="turn-outcome"
    data-role="agent"
    data-status={row.status}
  >
    <div class="flex items-center gap-2 text-xs font-semibold tracking-wide uppercase">
      <span class="text-fg" data-testid="turn-agent-name">{agentName(row.agent_id)}</span>
      {#if harness}<HarnessIcon {harness} testid="turn-harness-icon" />{:else}<Badge>?</Badge>{/if}
    </div>
    <div class="border-l-[0.5px] pl-3" style:border-left-color={agentBorderColor(row.agent_id)}>
      {#if row.status === "cancelled"}
        <StatusChip status="cancelled" testid="outcome-cancelled" />
      {:else}
        <StatusChip status="failed" testid="outcome-failed" />
        {#if row.reason}<span class="text-muted text-xs"> — {row.reason}</span>{/if}
      {/if}
    </div>
    {@render messageMeta({ at: row.at, mt: "mt-2.5" })}
  </div>
{/snippet}

{#snippet queuedRow(agentId: string, sendId: string)}
  <div class="space-y-1.5" data-testid="turn" data-role="agent">
    <div class="flex items-center gap-2 text-xs font-semibold tracking-wide uppercase">
      <span class="text-fg" data-testid="turn-agent-name">{agentName(agentId)}</span>
      {#if agentById[agentId]?.harness}
        <HarnessIcon harness={agentById[agentId]!.harness} testid="turn-harness-icon" />
      {/if}
    </div>
    <div class="border-l-[0.5px] pl-3" style:border-left-color={agentBorderColor(agentId)}>
      {@render queuedFooter(agentId, sendId, "turn-queued", "turn-live-control")}
    </div>
  </div>
{/snippet}

<!-- A held cross-agent forward: the user's typed body (verbatim, if any) plus a
     "waiting for {sources}…" footer — distinct copy from a busy recipient's
     "Queued…" — and a cancel control. Cancelling fires `cancel_forward`; the
     compose bar's awaiting `forward_message` then resolves cancelled, removes
     this entry, and restores the composer (text + source chips). -->
{#snippet heldForwardRow(held: HeldForward, recipientsHere: string[])}
  {@const sourceNames = held.sources.map((s) => s.name).join(", ")}
  {@const recipientNames = recipientsHere.map((id) => agentName(id)).join(", ")}
  <div class="group min-w-0 flex-1" data-testid="held-forward" data-forward-id={held.forwardId}>
    {#if held.body.trim() !== ""}
      <div
        class="border-accent/60 w-full max-w-full overflow-hidden rounded-xl border-l-2 bg-blue-100/20 px-4 py-2"
      >
        <Markdown text={held.body} />
      </div>
    {/if}
    <div class="text-muted mt-2 flex items-center gap-2 text-xs" data-testid="held-forward-waiting">
      <span class="animate-pulse">↪ Forward to {recipientNames} — waiting for {sourceNames}…</span>
      {@render liveTurnControl(
        () => void cancelForward(held.forwardId),
        `Cancel forward (waiting for ${sourceNames})`,
        "held-forward-cancel",
      )}
    </div>
  </div>
{/snippet}

{#snippet agentRow(turn: AgentTurn)}
  {@const harness = agentById[turn.agent_id]?.harness}
  {@const copyable = copyTextOf(turn, agentCopy.mode)}
  <!-- A non-completed Outcome marker (rendered as a sibling `outcomeRow`) is
       authoritative for this turn's status, mirroring the fan-out column. When
       present, suppress the turn's own status chip and the live footer so a
       cancelled-mid turn doesn't reopen with a phantom spinner (Claude
       `streaming`) or a contradictory `failed` chip (Codex/Gemini/Antigravity). -->
  {@const ownedByOutcome = hasOutcomeFor(turn)}
  <!-- Compact preview applies to any terminal response with content (complete,
       failed, cancelled, or dangling streaming-on-disk closed by a marker). Only
       a genuinely-live streaming turn is excluded — it uses the live-streaming cap. -->
  {@const previewEligible = isCollapsibleResponse(turn)}
  {@const key = `agent:${turn.turn_id}`}
  {@const latestResponse = latestResponseKeys.has(key)}
  {@const defaultCompact = compactEnabled && !latestResponse}
  {@const compact = previewEligible && isCompact(projectId, key, defaultCompact)}
  {@const showToggle = previewEligible && responseHasMore(turn, key, latestResponse)}
  <div class="group space-y-1.5" data-testid="turn" data-role="agent">
    <div class="flex items-center gap-2 text-xs font-semibold tracking-wide uppercase">
      <span class="text-fg" data-testid="turn-agent-name">{agentName(turn.agent_id)}</span>
      {#if harness}<HarnessIcon {harness} testid="turn-harness-icon" />{:else}<Badge>?</Badge>{/if}
    </div>
    <div
      class="space-y-1.5 border-l-[0.5px] pl-3"
      style:border-left-color={agentBorderColor(turn.agent_id)}
    >
      {#if compact}
        {@const hiddenLabel = hiddenItemsLabel(turn)}
        {#if hiddenLabel}{@render hiddenItemsIndicator(key, hiddenLabel)}{/if}
        {#if latestResponse}
          {@render turnBody(turn, false, "final")}
        {:else}
          <div
            class={cn("space-y-1.5", PREVIEW_CLIP)}
            use:measureClip={key}
            data-testid="preview-clip"
          >
            {@render turnBody(turn, false, "answer")}
          </div>
        {/if}
      {:else}
        {@render turnBody(turn, !ownedByOutcome)}
      {/if}
      <!-- Terminal status chip last (after the indicator and the body), so the
           collapsed and expanded views agree. Outside any height clip → always
           visible. Suppressed when an Outcome marker owns the status. -->
      {#if !ownedByOutcome}{@render turnStatusLabel(turn.status)}{/if}
    </div>
    {@render messageMeta({
      at: turn.started_at,
      copyable,
      label: "Copy message",
      spend: turn.spend,
      costUsd: turn.usage?.total_cost_usd,
      model: modelOf(turn),
      effort: turn.effort,
      previewKey: showToggle ? key : undefined,
      previewDefaultCompact: defaultCompact,
    })}
  </div>
{/snippet}

<!-- An agent-scoped inter-turn marker (compaction). Attributed to its agent (name
     + harness icon + the agent's lane border), then rendered as a tool-style
     `Disclosure` (gray box, chevron, collapsed by default) — the recap is a large
     verbatim harness block the user rarely needs expanded. No status icon: a
     compaction has no success/error state, so the header carries only the label.
     NOT a project-wide centered divider — in a multi-agent project that would
     misread as "the project compacted" and sever the per-agent lanes. -->
{#snippet systemMarkerRow(row: Extract<UnifiedRow, { kind: "system_marker" }>)}
  {@const harness = agentById[row.agent_id]?.harness}
  <div
    class="group space-y-1.5"
    data-testid="system-marker"
    data-role="agent"
    data-agent-id={row.agent_id}
  >
    <div class="flex items-center gap-2 text-xs font-semibold tracking-wide uppercase">
      <span class="text-fg" data-testid="turn-agent-name">{agentName(row.agent_id)}</span>
      {#if harness}<HarnessIcon {harness} testid="turn-harness-icon" />{/if}
    </div>
    <div class="border-l-[0.5px] pl-3" style:border-left-color={agentBorderColor(row.agent_id)}>
      {#if row.marker.marker_kind === "compaction"}
        <Disclosure testid="compaction-marker">
          {#snippet header()}
            <span class="text-muted shrink-0 text-[10px] font-semibold tracking-wide uppercase">
              Conversation compacted
            </span>
          {/snippet}
          <div class="border-border/70 border-t px-2.5 py-2">
            <pre
              class="text-muted max-h-44 overflow-y-auto font-mono text-xs whitespace-pre-wrap">{row
                .marker.summary}</pre>
          </div>
        </Disclosure>
      {:else if row.marker.marker_kind === "slash_command"}
        <!-- A state-changing slash command the harness ran (e.g. `/compact`).
             Shown so the user sees it happened; carries no correlating content. -->
        <div
          class="text-muted flex items-center gap-1.5 text-[10px] font-semibold tracking-wide uppercase"
          data-testid="slash-command-marker"
        >
          Ran <span class="font-mono normal-case">{row.marker.command}</span>
        </div>
      {/if}
      <!-- Unknown marker_kind: render nothing (degrade gracefully on a future
           variant this build doesn't model). -->
    </div>
    <!-- Same hover-revealed timestamp + copy as every other row (copies the
         verbatim recap). Also restores the inter-row spacing the bare disclosure
         lost. -->
    {@render messageMeta({
      at: row.at,
      copyable:
        row.marker.marker_kind === "compaction"
          ? row.marker.summary
          : row.marker.marker_kind === "slash_command"
            ? row.marker.command
            : "",
      label: row.marker.marker_kind === "slash_command" ? "Copy command" : "Copy summary",
      mt: "mt-2.5",
    })}
  </div>
{/snippet}

<div
  bind:this={container}
  onscroll={onScroll}
  data-testid="unified-transcript"
  class="bg-transcript [container-type:size] flex-1 overflow-y-auto px-8 py-4"
>
  {#if loadStatus === "loading" && rows.length === 0}
    <!-- Same centered spinner+title presentation as the project-loading
         EmptyState, so the two sequential loading states the user sees on a
         project switch don't jump between screen regions. -->
    <div
      class="flex h-full flex-col items-center justify-center gap-3 text-center"
      data-testid="transcript-loading"
    >
      <Spinner class="h-8 w-8" />
      <p class="text-muted text-sm">Loading history…</p>
    </div>
  {:else if loadStatus === "loading"}
    <!-- Rows are already on screen (per-agent hydration landed first) — a
         small note above them, not a centered takeover over visible content. -->
    <p class="text-muted mb-3 text-xs italic" data-testid="transcript-loading">Loading history…</p>
  {:else if loadStatus === "failed"}
    {@render failureBanner(
      "transcript-load-failed",
      "Couldn't load this project's conversation history.",
      onRetryLoad,
      () =>
        openDetails(
          "Couldn't load conversation history",
          "The project's conversation history failed to load. The exact error is below — copy it into a bug report.",
          loadError ?? "No error detail was reported.",
        ),
    )}
  {/if}

  <!-- Per-agent history failures: pinned chrome (not interleaved turns), one
       per failing roster agent. A failed agent contributes no turns to anchor
       against, so this lives at the top of the stream where the user is looking
       rather than mid-conversation. -->
  {#each failedAgents as agent (agent.id)}
    {@render failureBanner(
      "agent-hydration-failed",
      `Couldn't load ${agent.name}'s history.`,
      () => void retryAgentHydration(agent.id),
      () =>
        openDetails(
          `Couldn't load ${agent.name}'s history`,
          "This agent's history failed to load. The exact error is below — copy it into a bug report.",
          runtimes[agent.id]?.hydration_error ?? "No error detail was reported.",
        ),
    )}
  {/each}

  {#if rows.length === 0 && loadStatus === "complete" && failedAgents.length === 0}
    {#if showOnboarding}
      <!-- Orientation block for a blank project. Leads with the mental model
           (agents are isolated conversations; this view merges them) because
           that's the part of the product a chat-shaped UI actively miscues,
           then the three verbs (send, fan out, forward). Self-dismissing: it
           vanishes with the first rendered row, so no persistence needed. -->
      {#snippet kbd(text: string)}
        <kbd
          class="border-border bg-panel text-fg rounded border px-1 py-px font-mono text-[10px] whitespace-nowrap"
          >{text}</kbd
        >
      {/snippet}
      <!-- External authoring-guide link. Shows the full URL (not a pretty
           label) on purpose: the intended use is pasting it into a message so
           an agent can fetch the guide, so the raw string is the payload. -->
      {#snippet guideLink(url: string)}
        <button
          type="button"
          class="text-accent text-left break-all hover:underline"
          onclick={() =>
            void openExternalUrl(url).catch((err: unknown) => {
              console.error("[switchboard] open link failed", err);
            })}
        >
          {url}
        </button>
      {/snippet}
      <div
        class="mx-auto flex w-full max-w-xl flex-col gap-5 px-4 py-8"
        data-testid="transcript-onboarding"
      >
        <div class="flex flex-col gap-1">
          <p class="text-fg text-sm font-semibold">How this project works</p>
          <p class="text-muted text-xs leading-5">
            Each agent is one of the coding CLIs already installed on your machine — Switchboard
            runs it for you, using your existing login and configuration. The CLIs do the actual
            work; Switchboard is the window onto them.
          </p>
        </div>
        <ul class="flex flex-col gap-4">
          <li class="flex gap-3">
            <MessagesSquare
              size={16}
              strokeWidth={1.8}
              aria-hidden="true"
              class="text-muted mt-0.5 shrink-0"
            />
            <div class="flex min-w-0 flex-col gap-0.5">
              <p class="text-fg text-xs font-medium">Each agent is its own conversation</p>
              <p class="text-muted text-xs leading-5">
                Every agent's CLI session keeps its own private history and context window, just
                like a session in your terminal. An agent only ever sees the messages you send
                <em>it</em> — never what you send the others. What you're reading in this unified view
                is all of those conversations merged together, with every reply labeled by the agent it
                came from.
              </p>
            </div>
          </li>
          <li class="flex gap-3">
            <Columns2
              size={16}
              strokeWidth={1.8}
              aria-hidden="true"
              class="text-muted mt-0.5 shrink-0"
            />
            <div class="flex min-w-0 flex-col gap-0.5">
              <p class="text-fg text-xs font-medium">Split the view into panes</p>
              <p class="text-muted text-xs leading-5">
                By default, every agent shares this unified view. Panes let you regroup it: each
                pane shows only the agents you place in it, side by side with the others. Add a pane
                with the + button in the title bar, or move an agent into its own pane from its ⋯
                menu in the agents sidebar. Panes change only what you see — a message still goes to
                whichever agents you select.
              </p>
            </div>
          </li>
          <li class="flex gap-3">
            <Send
              size={16}
              strokeWidth={1.8}
              aria-hidden="true"
              class="text-muted mt-0.5 shrink-0"
            />
            <div class="flex min-w-0 flex-col gap-0.5">
              <p class="text-fg text-xs font-medium">Pick recipients, then send</p>
              <p class="text-muted text-xs leading-5">
                A message goes only to the agents selected in the <span class="text-fg font-medium"
                  >To</span
                >
                row — click the chips, press {@render kbd(`${shortcut("mod", "1")}–9`)}, or type
                {@render kbd("@name")}. Idle agents start immediately; busy agents queue the message
                and run it when they finish.
              </p>
            </div>
          </li>
          <li class="flex gap-3">
            <!-- Custom fan-out glyph (one stem splitting into three branches,
                 flowing left to right) — Lucide has no one-to-many icon that
                 isn't a node graph. Drawn in Lucide's stroke style so it sits
                 flush with the sibling icons. -->
            <svg
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              stroke-width="1.8"
              stroke-linecap="round"
              stroke-linejoin="round"
              aria-hidden="true"
              class="text-muted mt-0.5 h-4 w-4 shrink-0"
            >
              <path d="M3 12h6" />
              <path d="M9 12c4 0 4-7 12-7" />
              <path d="M9 12h12" />
              <path d="M9 12c4 0 4 7 12 7" />
            </svg>
            <div class="flex min-w-0 flex-col gap-0.5">
              <p class="text-fg text-xs font-medium">Send to several agents at once</p>
              <p class="text-muted text-xs leading-5">
                Select several recipients and send once — the same message is sent to each agent,
                and each works on it independently in its own conversation. Use it to compare
                different approaches to one task, or to put several agents on the same job in
                parallel — for example, multiple reviewers each examining the same change. The
                replies render side by side under your message.
              </p>
            </div>
          </li>
          <li class="flex gap-3">
            <CornerUpRight
              size={16}
              strokeWidth={1.8}
              aria-hidden="true"
              class="text-muted mt-0.5 shrink-0"
            />
            <div class="flex min-w-0 flex-col gap-0.5">
              <p class="text-fg text-xs font-medium">Relay with Forward</p>
              <p class="text-muted text-xs leading-5">
                <span class="text-fg font-medium">↪ Forward</span> sends an agent's latest reply to other
                agents — for example, have one CLI review another's plan. If the source agent is still
                working, the forward waits and delivers automatically when it finishes.
              </p>
            </div>
          </li>
          <li class="flex gap-3">
            <SquareSlash
              size={16}
              strokeWidth={1.8}
              aria-hidden="true"
              class="text-muted mt-0.5 shrink-0"
            />
            <div class="flex min-w-0 flex-col gap-0.5">
              <p class="text-fg text-xs font-medium">Reuse saved prompts</p>
              <p class="text-muted text-xs leading-5">
                Type {@render kbd("/")} in an empty message box — or click
                <span class="text-fg font-medium">Prompt</span> — to insert a saved prompt: a
                reusable message template with fill-in fields, for anything you find yourself
                retyping. Prompts are markdown files in a folder shared across all your projects —
                open it with "Open local prompts folder…" in the Prompt menu. You can also pull
                prompts from remote MCP servers (e.g. a hosted prompt library): add them under
                "Prompt servers (MCP)" in Settings. An agent can write a local one for you; point it
                at the authoring guide:
                {@render guideLink(
                  "https://github.com/shane-kercheval/switchboard/blob/main/docs/agent-instructions/prompts.md",
                )}
              </p>
            </div>
          </li>
          <li class="flex gap-3">
            <Workflow
              size={16}
              strokeWidth={1.8}
              aria-hidden="true"
              class="text-muted mt-0.5 shrink-0"
            />
            <div class="flex min-w-0 flex-col gap-0.5">
              <p class="text-fg text-xs font-medium">Automate with workflows</p>
              <p class="text-muted text-xs leading-5">
                Click <span class="text-fg font-medium">Workflow</span> to run a multi-step sequence
                over your agents — sends, forwards from one agent to another, and pauses for your
                review, defined in a YAML file. Workflow files live in a folder shared across all
                your projects — open it with "Open local workflows folder…" in the Workflow menu.
                The best way to create one is to have an agent write it for you; point it at the
                authoring guide:
                {@render guideLink(
                  "https://github.com/shane-kercheval/switchboard/blob/main/docs/agent-instructions/workflows.md",
                )}
              </p>
            </div>
          </li>
        </ul>
      </div>
    {:else}
      <p class="text-muted text-sm">No messages yet. Type a prompt below.</p>
    {/if}
  {/if}

  <!-- Upward-reveal sentinel: older history exists above the window. Kept OUTSIDE
       `content` so `captureAnchor`'s `content.children` scan never anchors on it. -->
  {#if firstVisibleIndex > 0}
    <div
      bind:this={revealSentinel}
      data-testid="reveal-sentinel"
      class="text-muted flex items-center justify-center gap-2 py-3 text-xs"
    >
      {#if revealing}<Spinner class="h-4 w-4" />{/if}
      <span>Earlier messages</span>
    </div>
  {/if}

  <div bind:this={content} class="space-y-5">
    {#each visibleBlocks as block (block.kind === "fanout" ? block.key : block.row.key)}
      <div data-testid="transcript-block">
        {#if block.kind === "row"}
          {#if block.row.kind === "user"}
            {@render userMessage(block.row)}
            {#if block.row.send_id !== undefined && block.row.agent_ids.length === 1 && queuedSendIds.has(block.row.send_id)}
              {@render queuedRow(block.row.agent_ids[0]!, block.row.send_id)}
            {/if}
          {:else if block.row.kind === "outcome"}
            {@render outcomeRow(block.row)}
          {:else if block.row.kind === "system_marker"}
            {@render systemMarkerRow(block.row)}
          {:else}
            {@render agentRow(block.row.turn)}
          {/if}
        {:else}
          {@const fanoutEntries = block.columns
            .filter((col) => isCollapsibleColumn(col.rows))
            .map((col) => {
              const key = `fanout:${block.send_id}:${col.agent_id}`;
              return { key, defaultCompact: compactEnabled && !latestResponseKeys.has(key) };
            })}
          {@const fanoutCopyable = fanoutText(block.columns)}
          {@const fanoutLiveCap = !block.columns.some((col) => {
            const state = columnState(col.rows);
            return state !== "queued" && state !== "streaming";
          })}
          <div class="space-y-4" data-testid="fanout-group">
            {@render userMessage(block.user)}
            <!-- The group control lives with the responses (not the user prompt)
               and shares a named hover scope with them, so it reveals when the
               responses are hovered and the user message's own hover chrome
               stays independent. -->
            <div class="group/responses space-y-2">
              <!-- Side-by-side on wide viewports; stacks vertically below `lg`. -->
              <div
                class="grid gap-4"
                style:grid-template-columns={`repeat(${block.columns.length}, minmax(0, 1fr))`}
              >
                {#each block.columns as col (col.agent_id)}
                  {@const state = columnState(col.rows)}
                  {@const harness = agentById[col.agent_id]?.harness}
                  {@const colCopyable = columnText(col.rows)}
                  <!-- A non-completed Outcome marker is authoritative for the
                   column's status and renders its own chip below; suppress the
                   turn's own status chip so a cancelled-mid turn the harness
                   persisted as `failed` doesn't show a contradictory `failed`
                   chip alongside the marker's `cancelled` (nor a redundant
                   doubled `failed`). Safe per the single-(send_id, agent_id)
                   column invariant. -->
                  {@const colHasOutcome = col.rows.some((r) => r.kind === "outcome")}
                  {@const colKey = `fanout:${block.send_id}:${col.agent_id}`}
                  {@const colEligible = isCollapsibleColumn(col.rows)}
                  {@const colLatestResponse = latestResponseKeys.has(colKey)}
                  {@const colDefaultCompact = compactEnabled && !colLatestResponse}
                  {@const colCompact =
                    colEligible && isCompact(projectId, colKey, colDefaultCompact)}
                  {@const colShowToggle =
                    colEligible &&
                    col.rows.some(
                      (r) =>
                        r.kind === "agent" && responseHasMore(r.turn, colKey, colLatestResponse),
                    )}
                  <div
                    class="group space-y-1.5"
                    data-testid="fanout-column"
                    data-role="agent"
                    data-agent-id={col.agent_id}
                    data-state={state}
                  >
                    <div
                      class="flex items-center gap-2 text-xs font-semibold tracking-wide uppercase"
                    >
                      <span class="text-fg" data-testid="turn-agent-name"
                        >{agentName(col.agent_id)}</span
                      >
                      {#if harness}<HarnessIcon {harness} />{/if}
                    </div>
                    <div
                      class="space-y-1.5 border-l-[0.5px] pl-3"
                      style:border-left-color={agentBorderColor(col.agent_id)}
                    >
                      {#if state === "queued"}
                        {@render queuedFooter(
                          col.agent_id,
                          block.send_id,
                          "fanout-queued",
                          "fanout-card-cancel",
                        )}
                      {/if}
                      {#if colCompact}
                        {@const colHiddenLabel = columnHiddenLabel(col.rows)}
                        {#if colHiddenLabel}
                          {@render hiddenItemsIndicator(colKey, colHiddenLabel)}
                        {/if}
                        {#if colLatestResponse}
                          {#each col.rows as r (r.key)}
                            {#if r.kind === "agent"}{@render turnBody(r.turn, false, "final")}{/if}
                          {/each}
                        {:else}
                          <div
                            class={cn("space-y-1.5", PREVIEW_CLIP)}
                            use:measureClip={colKey}
                            data-testid="preview-clip"
                          >
                            {#each col.rows as r (r.key)}
                              {#if r.kind === "agent"}{@render turnBody(
                                  r.turn,
                                  false,
                                  "answer",
                                )}{/if}
                            {/each}
                          </div>
                        {/if}
                        <!-- Status chip(s) last (after the indicator + body), and
                           outside the clip so a collapsed terminal column keeps
                           its outcome signal — matching the expanded order. -->
                        {@render columnStatusChips(col.rows, colHasOutcome)}
                      {:else}
                        {#each col.rows as r (r.key)}
                          {#if r.kind === "agent"}{@render turnBody(
                              r.turn,
                              state === "streaming",
                              "full",
                              fanoutLiveCap,
                            )}{/if}
                        {/each}
                        {@render columnStatusChips(col.rows, colHasOutcome)}
                      {/if}
                    </div>
                    {@render messageMeta({
                      at: columnAt(col.rows),
                      copyable: colCopyable,
                      label: `Copy ${agentName(col.agent_id)}'s message`,
                      model: columnModel(col.rows),
                      effort: columnEffort(col.rows),
                      previewKey: colShowToggle ? colKey : undefined,
                      previewDefaultCompact: colDefaultCompact,
                    })}
                  </div>
                {/each}
              </div>
              {#if fanoutEntries.length > 0 || fanoutCopyable.length > 0}
                <!-- Reveal toggles opacity with NO transition: on macOS WebKit a
                     settled `transition-opacity` leaves the leave-change unpainted
                     until a reflow (the footer stuck visible until a resize). The
                     other hover-reveals here (message meta) already toggle opacity
                     instantly for the same reason. -->
                <div
                  class="pointer-events-none flex items-center gap-2 pt-0.5 opacity-0 group-focus-within/responses:pointer-events-auto group-focus-within/responses:opacity-100 group-hover/responses:pointer-events-auto group-hover/responses:opacity-100"
                  data-testid="fanout-actions-footer"
                >
                  <div class="border-border/60 h-px min-w-0 flex-1 border-t"></div>
                  <div class="flex shrink-0 items-center gap-1">
                    {#if fanoutCopyable.length > 0}
                      <Tooltip label="Copy all responses above" side="bottom">
                        {#snippet trigger(props)}
                          <span {...props} class="inline-flex shrink-0">
                            <CopyButton
                              text={fanoutCopyable}
                              label="Copy all responses above"
                              testid="fanout-copy"
                              class="shrink-0"
                            />
                          </span>
                        {/snippet}
                      </Tooltip>
                    {/if}
                    {#if fanoutEntries.length > 0}{@render fanoutToggleAll(fanoutEntries)}{/if}
                  </div>
                </div>
              {/if}
            </div>
          </div>
        {/if}
      </div>
    {/each}
    <!-- Held cross-agent forwards: submitted but still waiting on their source
         agents' turns to settle. Render at the bottom (newest pending action),
         in the recipient's pane(s) only, distinct from a "Queued…" send — a held
         forward issued no `send_message` yet, so it has no `pending_sends` entry;
         it lives in the project-keyed `heldForwards` store (survives navigation,
         lost on restart). -->
    {#each heldForwardsHere as entry (entry.held.forwardId)}
      {@render heldForwardRow(entry.held, entry.recipients)}
    {/each}
  </div>
</div>

<ErrorDetailsDialog
  bind:open={detailsOpen}
  title={detailsTitle}
  message={detailsMessage}
  details={detailsText}
/>
