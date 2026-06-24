<script lang="ts">
  import type {
    RecipientRef,
    WorkflowStepInfo,
    WorkflowInputValue,
    WorkflowRunStatus,
  } from "$lib/types";
  import Spinner from "$lib/components/ui/Spinner.svelte";
  import { cn } from "$lib/utils";

  /// The ordered step list for a workflow, in two modes:
  ///  - `preview` (in the composer): every row neutral; slot recipients resolve
  ///    live against the form's `inputs` as the user assigns agents.
  ///  - `live` (replacing compose during a run): per-step done / active / pending /
  ///    failed state, derived here from the run's `current` index + `status` (the
  ///    shared convention — callers pass raw run fields, not per-step states).
  /// One component so preview and live never diverge in how a step reads.
  type Props = {
    steps: WorkflowStepInfo[];
    mode: "preview" | "live";
    /// Preview only: the form's bound input values, for resolving `slot` recipients.
    inputs?: Record<string, WorkflowInputValue>;
    /// Live only: the run's current/failing step index and status.
    current?: number;
    status?: WorkflowRunStatus;
    reason?: string | null;
  };

  let { steps, mode, inputs, current = 0, status = "running", reason = null }: Props = $props();

  type StepState = "done" | "active" | "pending" | "failed" | "preview";

  /// A displayed node: one unit of work. A `send` and the `wait` that
  /// synchronizes it collapse into a single node so the view reads as a pipeline
  /// of deliverables, not "Send…/Wait for…" mechanics. A node owns the *range* of
  /// physical step indices it absorbed (`[startStep, endStep]`) — the live state
  /// maps the run's single `current` index into that range, so two concurrent
  /// nodes (overlapping ranges) both read active = two spinners.
  type DisplayNode = {
    label: string;
    description?: string | null;
    recipients: RecipientRef[];
    feeds_from: RecipientRef[];
    startStep: number;
    endStep: number;
  };

  /// Identity of a recipient ref for collapse matching — independent of preview
  /// slot-resolution, so a send and its wait match whether recipients are still
  /// slots (preview) or resolved literals (live).
  function refKey(r: RecipientRef): string {
    return r.kind === "literal" ? `l:${r.name}` : `s:${r.input}`;
  }
  function refSet(refs: RecipientRef[]): Set<string> {
    return new Set(refs.map(refKey));
  }
  function setEq(a: Set<string>, b: Set<string>): boolean {
    return a.size === b.size && [...a].every((x) => b.has(x));
  }
  function isSubset(a: Set<string>, b: Set<string>): boolean {
    return [...a].every((x) => b.has(x));
  }

  type OpenSend = { node: DisplayNode; agents: Set<string> };

  /// Match a wait to the open send(s) it synchronizes. Equality (one send whose
  /// recipients exactly equal the wait — FIFO when an agent set was sent to more
  /// than once) covers linear flows and list-send→`wait_for_all`. Set-cover (a
  /// `wait_for_all` whose recipients are the union of several open single sends)
  /// covers the heterogeneous diamond. Anything else returns `[]` → the wait is
  /// **not** collapsed and renders as its own honest row (a wrong-but-honest
  /// display beats confidently mis-rendering sends as fire-and-forget).
  function matchOpens(open: OpenSend[], want: Set<string>): OpenSend[] {
    const eq = open.find((o) => setEq(o.agents, want));
    if (eq) return [eq];
    const subset = open.filter((o) => isSubset(o.agents, want));
    if (subset.length === 0) return [];
    const union = new Set(subset.flatMap((o) => [...o.agents]));
    return setEq(union, want) ? subset : [];
  }

  /// Fold the physical steps into display nodes. `send` opens a node; a matching
  /// `wait` closes it (absorbing the wait's index into the node's range);
  /// everything else (unmatched wait, `pause`, `for_each`, or an `unknown`/legacy
  /// kind) renders as its own row. A send never closed = fire-and-forget.
  const nodes = $derived.by<DisplayNode[]>(() => {
    const out: DisplayNode[] = [];
    let open: OpenSend[] = [];
    const ownRow = (s: WorkflowStepInfo, i: number): DisplayNode => {
      const node: DisplayNode = {
        label: s.label,
        description: s.description,
        recipients: s.recipients,
        feeds_from: s.feeds_from,
        startStep: i,
        endStep: i,
      };
      out.push(node);
      return node;
    };
    steps.forEach((s, i) => {
      const kind = s.kind ?? "unknown";
      if (kind === "send") {
        open.push({ node: ownRow(s, i), agents: refSet(s.recipients) });
      } else if (kind === "wait") {
        const matched = matchOpens(open, refSet(s.recipients));
        if (matched.length > 0) {
          for (const m of matched) m.node.endStep = i;
          open = open.filter((o) => !matched.includes(o));
        } else {
          ownRow(s, i); // unmatched wait → honest row, not a silent mis-collapse
        }
      } else {
        ownRow(s, i);
      }
    });
    return out;
  });

  function nodeState(node: DisplayNode): StepState {
    if (mode === "preview") return "preview";
    if (current > node.endStep) return "done";
    if (current < node.startStep) return "pending";
    // `current` is inside the node's absorbed range. While running, that's two
    // concurrent spinners for overlapping (diamond) nodes — correct.
    if (status === "running") return "active";
    // Terminal run: attribute the failure by *ownership*, not range membership. A
    // node owns only its send (`startStep`) and its closing wait (`endStep`); the
    // indices between belong to interleaved sibling branches. So only the node
    // whose own step actually failed turns red — a concurrent sibling cut short by
    // a different branch's failure shows neutral `pending` (it never completed),
    // not a false `failed`. (A shared `wait_for_all` failing at `endStep`
    // correctly marks every node that absorbed it — they all own that index.)
    return current === node.startStep || current === node.endStep ? "failed" : "pending";
  }

  /// Resolve a row's recipient refs to display names. A `literal` is shown
  /// verbatim; a `slot` resolves against `inputs` in preview (and falls back to the
  /// input name when unbound), and in live mode is already a literal — an
  /// unresolved slot there just shows the input name.
  ///
  /// Agents are the first-class unit: a slot bound by selecting a *pane* resolves
  /// to its member agent names here — we deliberately do NOT collapse a pane's
  /// members back to a pane name. A pane is a selection convenience (and a
  /// keyboard shortcut), not a displayed entity, which keeps every recipient
  /// surface consistent and sidesteps stale/ambiguous pane references (pane
  /// membership is mutable; the run resolved to concrete agents at invoke).
  function names(refs: RecipientRef[]): string[] {
    return refs.flatMap((r) => {
      if (r.kind === "literal") return [r.name];
      const bound = inputs?.[r.input];
      if (bound === undefined) return [r.input];
      if (typeof bound === "string") return bound.trim() === "" ? [r.input] : [bound];
      return bound.length === 0 ? [r.input] : bound;
    });
  }
</script>

<ol class="flex flex-col gap-1.5" data-testid="workflow-steps">
  {#each nodes as node, i (i)}
    {@const state = nodeState(node)}
    {@const recipients = names(node.recipients)}
    {@const feeds = names(node.feeds_from)}
    <li
      class="flex items-start gap-2 text-sm"
      data-testid={`workflow-step-${i}`}
      data-step-state={state}
    >
      <span class="mt-0.5 flex h-3.5 w-3.5 shrink-0 items-center justify-center" aria-hidden="true">
        {#if state === "active"}
          <Spinner class="h-3.5 w-3.5" />
        {:else if state === "done"}
          <svg
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            stroke-width="2.5"
            stroke-linecap="round"
            stroke-linejoin="round"
            class="text-accent h-3.5 w-3.5"
          >
            <path d="M20 6 9 17l-5-5" />
          </svg>
        {:else if state === "failed"}
          <svg
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            stroke-width="2.5"
            stroke-linecap="round"
            stroke-linejoin="round"
            class="text-status-failed h-3.5 w-3.5"
          >
            <path d="M18 6 6 18M6 6l12 12" />
          </svg>
        {:else}
          <!-- pending / preview: a hollow, dim marker -->
          <span
            class={cn(
              "h-2 w-2 rounded-full border",
              state === "pending" ? "border-muted/40" : "border-muted/60",
            )}
          ></span>
        {/if}
      </span>
      <span class="flex min-w-0 flex-col gap-0.5">
        <span class="flex flex-wrap items-baseline gap-x-1.5">
          <span class={cn("font-medium", state === "pending" ? "text-muted" : "text-fg")}
            >{node.label}</span
          >
          {#if recipients.length > 0}
            <span class="text-muted text-xs" data-testid={`workflow-step-recipients-${i}`}>
              ({recipients.join(", ")})
            </span>
          {/if}
        </span>
        {#if node.description}
          <span class="text-muted text-xs" data-testid={`workflow-step-description-${i}`}>
            {node.description}
          </span>
        {/if}
        {#if feeds.length > 0}
          <span class="text-muted text-[11px] leading-4" data-testid={`workflow-step-feeds-${i}`}>
            ↪ feeds from {feeds.join(", ")}
          </span>
        {/if}
        {#if state === "failed" && reason}
          <span class="text-status-failed text-xs" data-testid={`workflow-step-reason-${i}`}
            >{reason}</span
          >
        {/if}
      </span>
    </li>
  {/each}
</ol>
