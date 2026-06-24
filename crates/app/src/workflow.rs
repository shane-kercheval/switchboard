//! The workflow interpreter: executes a parsed, bound [`Workflow`] against a
//! project's live agents by driving the real [`Dispatcher`]. It is a *conductor*
//! over app-owned machinery (the dispatcher, `PromptService`, run-record IO), so
//! it lives in `crates/app`, not the pure workflow crate.
//!
//! ## Model
//!
//! A step-based interpreter (not a DAG scheduler — v1 doesn't need one). It walks
//! the snapshotted steps and:
//!
//! - `send` dispatches each recipient via [`Dispatcher::send_message_awaiting_completion`]
//!   (`FailFast`), minting one `send_id` per step (shared across a fan-out's
//!   recipients), and keeps each turn's completion handle.
//! - `wait_for` / `wait_for_all` await those handles; on a `Completed` terminal
//!   the agent's resolved text enters the **in-memory per-run output scope** that
//!   `forward_from` and the template helpers read. The scope is never persisted
//!   (resume is deferred — §3 stands).
//! - `pause_for_user` / `for_each` are not executable in this version; the
//!   interpreter errors clearly (defense-in-depth — the invoke command gates them
//!   up front).
//!
//! ## Failure / cancel
//!
//! Workflow contention is `FailFast` → a `Busy` is a step failure (the deliberate
//! opposite of the manual compose bar's queue). A failed step marks the run
//! `failed` but never cancels surviving siblings (the non-destructive floor — they
//! run to their natural terminal in the dispatcher). A workflow-level cancel, or a
//! participating turn observed `Cancelled`, marks the run `cancelled` and fires
//! `CancelSource::Workflow` on the run's agents. The run does not go `complete`
//! until every turn it dispatched has settled (the trailing-settle hold); a
//! trailing failure marks it `failed`.
//!
//! Progress is recorded to `runs/<run-id>.jsonl` ([`RunRecord`]) so the app can
//! surface an active or interrupted run for abandon, and mirrored live through a
//! [`ProgressSink`] for the run indicator. No agent text is written or emitted.

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;

use chrono::Utc;
use switchboard_core::AgentId;
use switchboard_core::name::canonicalize_for_uniqueness;
use switchboard_dispatcher::{
    AwaitableSendOutcome, CompletionResult, DispatchContextFactory, Dispatcher,
};
use switchboard_harness::forward::{ForwardedBlock, compose_forwarded_message};
use switchboard_harness::{CancelSource, TurnOutcome};
use switchboard_prompts::{PromptId, PromptService};
use switchboard_workflow::{
    OutputScope, RunRecord, RunStatus, Scope, ScopeValue, SendStep, Step, Templated,
    TerminalStatus, UNSUPPORTED_STEP_MESSAGE, WaitForAllStep, WaitForStep, Workflow, render,
    resolve_agent_refs, step_display, validate_agent_list,
};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Produces the per-send [`DispatchContextFactory`] for an agent. The production
/// implementation builds a `ProjectDispatchContextFactory` from `AppState`;
/// tests provide a mock-backed one. This is the single seam the interpreter needs
/// to dispatch without depending on `AppState` internals — it is integration-
/// tested through the real `Dispatcher` + `MockHarnessAdapter`, not a mock of the
/// dispatcher.
pub trait DispatchFactoryProvider: Send + Sync {
    /// The per-send factory for `agent_id`, or `None` if the agent is no longer
    /// resolvable (e.g. removed mid-run). The interpreter turns `None` into a
    /// clean step failure rather than panicking — `factory_for` is called only
    /// with the run's bound agents, so `None` is the rare removed-mid-run case.
    fn factory_for(&self, agent_id: AgentId) -> Option<Arc<dyn DispatchContextFactory>>;
}

/// A live progress event emitted by the interpreter as a run advances. It carries
/// **no run/project identity** (the app's sink attaches `run_id`/`project_id`
/// when mapping onto the `workflow:<project-id>` channel) and **no agent output
/// text** (§3 — the turns themselves stream on `agent:<id>` and render in the
/// transcript; the indicator only tracks orchestration). Emission points coincide
/// with the on-disk record boundaries (start, terminal) plus one step-start, so
/// live events and the run file cannot drift.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkflowProgress {
    /// The run has begun: its workflow name and total top-level step count.
    Started {
        workflow: String,
        total_steps: usize,
    },
    /// A top-level step has *begun* (zero-based index) — so the indicator advances
    /// when a step starts, not only when the prior one completes.
    StepStarted { step_index: usize },
    /// The run reached a controlled terminal, with the failing step and reason on
    /// a failed run.
    Terminal {
        status: TerminalStatus,
        failed_step: Option<usize>,
        reason: Option<String>,
    },
}

/// Receives the interpreter's live [`WorkflowProgress`] events. The production
/// sink maps them onto the project-scoped `workflow:<project-id>` channel; tests
/// supply a recorder. Separate from [`DispatchFactoryProvider`] because progress
/// is a fire-and-forget side-channel, not part of dispatch.
pub trait ProgressSink: Send + Sync {
    fn emit(&self, event: WorkflowProgress);
}

/// A sink that drops every event — for runs (and tests) that don't observe
/// progress.
pub struct NullProgressSink;

impl ProgressSink for NullProgressSink {
    fn emit(&self, _event: WorkflowProgress) {}
}

/// A single workflow run. Construct it with the bound snapshot + dependencies and
/// drive it to a terminal [`RunStatus`] with [`WorkflowRun::execute`]. Built by
/// the invoke command (and by integration tests) — all inputs are owned/`Arc` so
/// the run can move into a background task.
pub struct WorkflowRun {
    /// Immutable snapshot of the parsed workflow (captured at invocation).
    pub workflow: Workflow,
    /// Bound input values (from `bind_invocation`), the outermost render scope.
    pub inputs: BTreeMap<String, ScopeValue>,
    /// User-fillable prompt-argument values by name (auto-derived args the user
    /// filled, plus declared `text` inputs that feed a same-named prompt arg).
    /// Deliberately **not** in the `MiniJinja` workflow scope — these are prompt
    /// args, not template vars, so a derived arg can't shadow a `text:` template
    /// variable. A `send`'s prompt receives only the subset its schema declares.
    pub user_args: BTreeMap<String, String>,
    /// Canonical agent name → `AgentId` for every agent the workflow may target.
    pub agents: BTreeMap<String, AgentId>,
    pub dispatcher: Arc<Dispatcher>,
    pub prompts: PromptService,
    pub factories: Arc<dyn DispatchFactoryProvider>,
    /// `runs/<run-id>.jsonl` — where progress/terminal records are appended.
    pub run_path: PathBuf,
    /// Live progress side-channel: start / step-start / terminal events the app
    /// forwards to the run indicator. Disk records are the durable record; this is
    /// the live mirror.
    pub progress: Arc<dyn ProgressSink>,
    /// Fired by a workflow-level cancel; the run stops and is marked `cancelled`.
    pub cancel: CancellationToken,
}

/// How a single step ended unsuccessfully.
enum StepError {
    /// The step failed (contention, render error, missing forward output, an
    /// awaited turn's harness failure). Carries the operational reason.
    Failed(String),
    /// Execution was cancelled — a workflow-level cancel, or a participating
    /// turn observed `Cancelled`. The whole run is `cancelled`.
    Cancelled,
}

impl WorkflowRun {
    pub async fn execute(self) -> RunStatus {
        let mut outputs: OutputScope = OutputScope::new();
        // Per agent, the FIFO queue of dispatched turns not yet awaited. A queue
        // (not one slot) so a second send to an agent — accepted once its prior
        // turn is terminal, via the dispatcher's drain-window accept — does not
        // overwrite and lose the first turn's outcome: every dispatched turn is
        // awaited somewhere (a `wait_for` or trailing settle), so an earlier
        // failure is always observed. `wait_for` consumes the **oldest** queued
        // turn (`pop_front`); the output scope therefore reflects the turn the
        // workflow explicitly awaited ("most-recent-completed-turn-this-workflow-saw"),
        // which in the unusual multi-outstanding-same-agent case leaves a never-
        // awaited turn settled-for-failure-accounting but not absorbed into scope.
        let mut pending: BTreeMap<AgentId, VecDeque<oneshot::Receiver<CompletionResult>>> =
            BTreeMap::new();
        // Count of this run's turns currently in flight per agent (incremented on
        // dispatch, decremented when a turn's terminal is observed). This — not
        // the declared roster — is what a workflow cancel targets, so cancelling
        // never kills an agent's *unrelated* (manual / other-run) turn. Tracked
        // separately from `pending` because `wait_for_all` moves handles out of
        // `pending` while awaiting, so `pending` alone undercounts in flight.
        let mut in_flight: BTreeMap<AgentId, usize> = BTreeMap::new();

        self.record(&RunRecord::Started {
            workflow: self.workflow.name.clone(),
            total_steps: self.workflow.steps.len(),
            // Declared step snapshot, so a failed/interrupted run reconstructs its
            // progress view from the run file after a restart (the live registry's
            // resolved copy is gone once the process exits).
            steps: step_display(&self.workflow),
            at: Utc::now(),
        });
        self.progress.emit(WorkflowProgress::Started {
            workflow: self.workflow.name.clone(),
            total_steps: self.workflow.steps.len(),
        });

        // The steps are the immutable snapshot; clone so the walk doesn't borrow
        // `self` while the step executors take `&mut` of it.
        let steps = self.workflow.steps.clone();
        for (index, labeled) in steps.iter().enumerate() {
            if self.cancel.is_cancelled() {
                return self.finish_cancelled(&in_flight);
            }
            self.progress
                .emit(WorkflowProgress::StepStarted { step_index: index });
            let result = match &labeled.step {
                Step::Send(s) => {
                    self.execute_send(s, &outputs, &mut pending, &mut in_flight)
                        .await
                }
                Step::WaitFor(w) => {
                    self.execute_wait_for(w, &mut outputs, &mut pending, &mut in_flight)
                        .await
                }
                Step::WaitForAll(w) => {
                    self.execute_wait_for_all(w, &mut outputs, &mut pending, &mut in_flight)
                        .await
                }
                // These parse and invocation-validate as syntactically valid but
                // are gated out of this version at the invoke command; this branch
                // is defense-in-depth so a slipped-through run fails clearly with
                // the same message.
                Step::PauseForUser(_) | Step::ForEach(_) => {
                    Err(StepError::Failed(UNSUPPORTED_STEP_MESSAGE.to_owned()))
                }
            };
            match result {
                Ok(()) => self.record(&RunRecord::StepCompleted {
                    step_index: index,
                    at: Utc::now(),
                }),
                Err(StepError::Failed(reason)) => return self.finish_failed(Some(index), reason),
                Err(StepError::Cancelled) => return self.finish_cancelled(&in_flight),
            }
        }

        // Trailing settle: hold the run open until every dispatched turn settles,
        // including fire-and-forget sends with no trailing wait.
        match self.settle_remaining(&mut pending, &mut in_flight).await {
            Ok(()) => self.finish_terminal(TerminalStatus::Complete, None, None),
            Err(StepError::Failed(reason)) => self.finish_failed(None, reason),
            Err(StepError::Cancelled) => self.finish_cancelled(&in_flight),
        }
    }

    async fn execute_send(
        &self,
        step: &SendStep,
        outputs: &OutputScope,
        pending: &mut BTreeMap<AgentId, VecDeque<oneshot::Receiver<CompletionResult>>>,
        in_flight: &mut BTreeMap<AgentId, usize>,
    ) -> Result<(), StepError> {
        let recipients = self.resolve_agent_ids(&step.to, outputs)?;
        let body = self.build_body(step, outputs).await?;
        // One send_id for the whole step; a fan-out's recipients share it so the
        // user-facing send renders once, exactly like a manual multi-recipient send.
        let send_id = Uuid::now_v7();
        for agent_id in recipients {
            let factory = self.factories.factory_for(agent_id).ok_or_else(|| {
                StepError::Failed(
                    "a participating agent is unavailable for dispatch (removed mid-run, or its harness is unsupported)".to_owned(),
                )
            })?;
            match self
                .dispatcher
                .send_workflow_message_awaiting_completion(
                    agent_id,
                    &body,
                    Vec::new(),
                    send_id,
                    factory,
                )
                .await
            {
                AwaitableSendOutcome::Accepted { completion, .. } => {
                    pending.entry(agent_id).or_default().push_back(completion);
                    *in_flight.entry(agent_id).or_insert(0) += 1;
                    // The dispatcher emits this send's live user message once it is
                    // durable (after `record_send`, before `TurnStart`) — see
                    // `send_workflow_message_awaiting_completion`. A fan-out's
                    // recipients share `send_id`, so the frontend groups them into
                    // one user row + per-recipient columns, matching a manual send
                    // and the reloaded journal view.
                }
                // Contention = step failure (FailFast). Per the partial-dispatch
                // rule, remaining recipients are not dispatched and already-issued
                // turns are left to run to their natural terminal (not cancelled —
                // their output stays visible in the transcript).
                AwaitableSendOutcome::Busy => {
                    return Err(StepError::Failed(
                        "an agent is busy — workflow steps fail fast on contention".to_owned(),
                    ));
                }
            }
        }
        Ok(())
    }

    async fn execute_wait_for(
        &self,
        step: &WaitForStep,
        outputs: &mut OutputScope,
        pending: &mut BTreeMap<AgentId, VecDeque<oneshot::Receiver<CompletionResult>>>,
        in_flight: &mut BTreeMap<AgentId, usize>,
    ) -> Result<(), StepError> {
        let (name, agent_id) = self.resolve_single_agent(&step.agent, outputs)?;
        let rx = pending
            .get_mut(&agent_id)
            .and_then(VecDeque::pop_front)
            .ok_or_else(|| StepError::Failed(format!("no turn to wait on for agent `{name}`")))?;
        // On a cancel-race (`?` returns early) the turn is *not* observed, so its
        // in-flight count stays live for `finish_cancelled` to cancel.
        let result = self.await_completion(rx).await?;
        Self::observe(in_flight, agent_id);
        Self::absorb(&name, result, outputs)
    }

    async fn execute_wait_for_all(
        &self,
        step: &WaitForAllStep,
        outputs: &mut OutputScope,
        pending: &mut BTreeMap<AgentId, VecDeque<oneshot::Receiver<CompletionResult>>>,
        in_flight: &mut BTreeMap<AgentId, usize>,
    ) -> Result<(), StepError> {
        let agents = self.resolve_named_agent_ids(&step.agents, outputs)?;
        // Take every awaited turn's handle up front so a later missing one fails
        // before we block on the first.
        let mut waits = Vec::with_capacity(agents.len());
        for (name, agent_id) in agents {
            let rx = pending
                .get_mut(&agent_id)
                .and_then(VecDeque::pop_front)
                .ok_or_else(|| {
                    StepError::Failed(format!("no turn to wait on for agent `{name}`"))
                })?;
            waits.push((name, agent_id, rx));
        }
        // Await all (the non-destructive floor: a sibling failing does not stop
        // the others — every survivor settles and its output is retained), then
        // fail the step if any one failed. Cancellation short-circuits the run.
        let mut failure: Option<String> = None;
        for (name, agent_id, rx) in waits {
            let result = self.await_completion(rx).await?;
            Self::observe(in_flight, agent_id);
            match Self::absorb(&name, result, outputs) {
                Ok(()) => {}
                Err(StepError::Failed(reason)) => {
                    failure.get_or_insert(reason);
                }
                Err(StepError::Cancelled) => return Err(StepError::Cancelled),
            }
        }
        match failure {
            Some(reason) => Err(StepError::Failed(reason)),
            None => Ok(()),
        }
    }

    /// Mark one of `agent`'s in-flight turns as observed-terminal (decrementing
    /// the per-agent count, dropping the entry at zero). Called only after a turn
    /// actually settles, so a cancel that races the await leaves the count live.
    fn observe(in_flight: &mut BTreeMap<AgentId, usize>, agent: AgentId) {
        if let Some(count) = in_flight.get_mut(&agent) {
            *count -= 1;
            if *count == 0 {
                in_flight.remove(&agent);
            }
        }
    }

    /// Fold one awaited turn's outcome into the run: store a completed turn's text
    /// into the output scope, surface a failed turn as a step failure, and a
    /// cancelled turn as run cancellation.
    fn absorb(
        name: &str,
        result: CompletionResult,
        outputs: &mut OutputScope,
    ) -> Result<(), StepError> {
        match result.outcome {
            TurnOutcome::Completed => {
                outputs.insert(canonicalize_for_uniqueness(name), result.text);
                Ok(())
            }
            TurnOutcome::Failed { message, .. } => Err(StepError::Failed(format!(
                "agent `{name}` failed: {message}"
            ))),
            TurnOutcome::Cancelled { .. } => Err(StepError::Cancelled),
            // `TurnOutcome` is `#[non_exhaustive]`; a future terminal we don't yet
            // model is treated as a step failure rather than silently completing.
            _ => Err(StepError::Failed(format!(
                "agent `{name}` ended with an unrecognized outcome"
            ))),
        }
    }

    /// Await one completion, racing a workflow-level cancel. A dropped sender
    /// (which the dispatcher's drop paths now avoid) is treated as a failure
    /// rather than a hang.
    async fn await_completion(
        &self,
        rx: oneshot::Receiver<CompletionResult>,
    ) -> Result<CompletionResult, StepError> {
        tokio::select! {
            biased;
            () = self.cancel.cancelled() => Err(StepError::Cancelled),
            received = rx => received.map_err(|_| {
                StepError::Failed("dispatcher dropped the turn completion".to_owned())
            }),
        }
    }

    /// Trailing-settle: await every still-outstanding turn so the run does not go
    /// `complete` while a fire-and-forget send is in flight.
    async fn settle_remaining(
        &self,
        pending: &mut BTreeMap<AgentId, VecDeque<oneshot::Receiver<CompletionResult>>>,
        in_flight: &mut BTreeMap<AgentId, usize>,
    ) -> Result<(), StepError> {
        let mut failure: Option<String> = None;
        let outstanding: Vec<(AgentId, oneshot::Receiver<CompletionResult>)> =
            std::mem::take(pending)
                .into_iter()
                .flat_map(|(agent_id, queue)| queue.into_iter().map(move |rx| (agent_id, rx)))
                .collect();
        for (agent_id, rx) in outstanding {
            let result = self.await_completion(rx).await?;
            Self::observe(in_flight, agent_id);
            match result.outcome {
                TurnOutcome::Completed => {}
                TurnOutcome::Failed { message, .. } => {
                    failure.get_or_insert(format!("a trailing turn failed: {message}"));
                }
                TurnOutcome::Cancelled { .. } => return Err(StepError::Cancelled),
                _ => {
                    failure.get_or_insert(
                        "a trailing turn ended with an unrecognized outcome".to_owned(),
                    );
                }
            }
        }
        match failure {
            Some(reason) => Err(StepError::Failed(reason)),
            None => Ok(()),
        }
    }

    /// The declared argument names of a hardcoded prompt, via the prompt cache
    /// (the same `PromptService` lookup the form descriptor and invoke validation
    /// use). Empty if the prompt isn't cached — invoke pre-flight already gated an
    /// unresolved/incompatible workflow, so the schema is expected here; an empty
    /// result simply passes no user args and lets `render` (a live fetch for MCP)
    /// be the final authority.
    fn prompt_arg_names(&self, id: &PromptId) -> Vec<String> {
        self.prompts
            .get(&id.provider, &id.name)
            .map(|p| p.arguments.into_iter().map(|a| a.name).collect())
            .unwrap_or_default()
    }

    /// Build the dispatched body: the rendered `prompt`/`text` lead (if any), then
    /// each `forward_from` source's resolved output composed into a canonical
    /// block. `prompt` resolves through `PromptService`; `forward_from` reads the
    /// per-run output scope (the agent must have completed earlier this run).
    async fn build_body(
        &self,
        step: &SendStep,
        outputs: &OutputScope,
    ) -> Result<String, StepError> {
        let base = if let Some(prompt) = &step.prompt {
            // `send.prompt` is a hardcoded literal (parse-time invariant), so there
            // is nothing to render — parse it straight into a `PromptId`. Parsing
            // stays in the app layer; the pure workflow crate never sees `PromptId`.
            let id = PromptId::parse(prompt).map_err(|e| StepError::Failed(e.to_string()))?;

            // The prompt's args come from two sources: the workflow's computed
            // `template_vars` bindings, and the user-filled values for this
            // prompt's own user-fillable args. `render_template` rejects unknown
            // args, so we pass *only* the args this prompt declares — never the
            // whole user-value map (a leak would fail the run).
            let mut args: BTreeMap<String, String> = BTreeMap::new();
            for (name, template) in &step.template_vars {
                args.insert(
                    name.clone(),
                    render(template, self.scope(), outputs).map_err(render_failed)?,
                );
            }
            for arg in self.prompt_arg_names(&id) {
                if args.contains_key(&arg) {
                    continue; // already bound by a computed template_var
                }
                // Omit an unfilled/blank value so an optional arg's `{% if %}`
                // takes its empty branch instead of receiving "".
                if let Some(value) = self.user_args.get(&arg).filter(|v| !v.trim().is_empty()) {
                    args.insert(arg, value.clone());
                }
            }
            self.prompts
                .render(&id.provider, &id.name, &args)
                .await
                .map_err(|e| StepError::Failed(e.to_string()))?
                .text
        } else if let Some(text) = &step.text {
            render(text, self.scope(), outputs).map_err(render_failed)?
        } else {
            String::new()
        };

        let Some(forward) = &step.forward_from else {
            return Ok(base);
        };
        let names = resolve_agent_refs(forward, &self.scope(), outputs).map_err(render_failed)?;
        let mut blocks: Vec<(String, String)> = Vec::with_capacity(names.len());
        for name in &names {
            let key = canonicalize_for_uniqueness(name.trim());
            match outputs.get(&key) {
                Some(text) => blocks.push((name.trim().to_owned(), text.clone())),
                None => {
                    return Err(StepError::Failed(format!(
                        "no in-workflow completed output for agent `{}`",
                        name.trim()
                    )));
                }
            }
        }
        let block_refs: Vec<ForwardedBlock<'_>> = blocks
            .iter()
            .map(|(name, text)| ForwardedBlock {
                agent_name: name,
                text,
            })
            .collect();
        Ok(compose_forwarded_message(&base, &block_refs))
    }

    /// Resolve a `to` / `agents` field to recipient ids — validating the resolved
    /// list (non-empty, unique, all in the roster) per the spec, then mapping to
    /// `AgentId`s in declared order.
    fn resolve_agent_ids(
        &self,
        field: &Templated,
        outputs: &OutputScope,
    ) -> Result<Vec<AgentId>, StepError> {
        Ok(self
            .resolve_named_agent_ids(field, outputs)?
            .into_iter()
            .map(|(_, id)| id)
            .collect())
    }

    /// As [`Self::resolve_agent_ids`] but keeps each agent's canonical name (needed
    /// to key the output scope when its turn is awaited).
    fn resolve_named_agent_ids(
        &self,
        field: &Templated,
        outputs: &OutputScope,
    ) -> Result<Vec<(String, AgentId)>, StepError> {
        let names = resolve_agent_refs(field, &self.scope(), outputs).map_err(render_failed)?;
        let roster: HashSet<String> = self.agents.keys().cloned().collect();
        validate_agent_list(&names, &roster).map_err(|e| StepError::Failed(e.to_string()))?;
        Ok(names
            .iter()
            .map(|name| {
                let key = canonicalize_for_uniqueness(name.trim());
                let id = self.agents[&key];
                (key, id)
            })
            .collect())
    }

    /// Resolve a single-agent field (`wait_for`). Errors if it resolves to zero or
    /// more than one agent, or to one outside the roster.
    fn resolve_single_agent(
        &self,
        template: &str,
        outputs: &OutputScope,
    ) -> Result<(String, AgentId), StepError> {
        let names = resolve_agent_refs(
            &Templated::Scalar(template.to_owned()),
            &self.scope(),
            outputs,
        )
        .map_err(render_failed)?;
        match names.as_slice() {
            [name] => {
                let key = canonicalize_for_uniqueness(name.trim());
                let id = self.agents.get(&key).copied().ok_or_else(|| {
                    StepError::Failed(format!("agent `{}` does not exist", name.trim()))
                })?;
                Ok((key, id))
            }
            _ => Err(StepError::Failed("expected a single agent".to_owned())),
        }
    }

    fn scope(&self) -> Scope {
        // No `user_input` (no pause) and no iteration (no for_each) in this
        // version; the send's template_vars are rendered into prompt args here,
        // not layered into scope.
        Scope {
            inputs: self.inputs.clone(),
            ..Scope::default()
        }
    }

    /// Cancel the run's **own** in-flight turns with `CancelSource::Workflow` —
    /// only agents this run currently has a turn running on, never the whole
    /// declared roster (which would kill an agent's unrelated manual / other-run
    /// turn). Cancel is per-agent, so an inherent residual race remains: a
    /// workflow turn that finished but isn't yet observed (a fire-and-forget not
    /// yet settled) still counts here, so if a manual turn started on that agent
    /// in the gap, this can hit it. That window is narrow and uncloseable without
    /// per-turn cancellation; the fix narrows it from "all participants" to
    /// "agents with a live workflow turn."
    fn cancel_inflight(&self, in_flight: &BTreeMap<AgentId, usize>) {
        for (agent_id, count) in in_flight {
            if *count > 0 {
                self.dispatcher.cancel(*agent_id, CancelSource::Workflow);
            }
        }
    }

    fn finish_cancelled(&self, in_flight: &BTreeMap<AgentId, usize>) -> RunStatus {
        self.cancel_inflight(in_flight);
        self.finish_terminal(TerminalStatus::Cancelled, None, None)
    }

    fn finish_failed(&self, failed_step: Option<usize>, reason: String) -> RunStatus {
        // Per the non-destructive floor, surviving turns are *not* cancelled — they
        // settle in the dispatcher and their output stays visible.
        self.finish_terminal(TerminalStatus::Failed, failed_step, Some(reason))
    }

    /// Record the terminal to disk **and** emit the matching live progress event,
    /// from one place so the durable record and the live mirror can never drift.
    /// Returns the `RunStatus` the run resolves to.
    fn finish_terminal(
        &self,
        status: TerminalStatus,
        failed_step: Option<usize>,
        reason: Option<String>,
    ) -> RunStatus {
        self.record(&RunRecord::Terminal {
            status,
            failed_step,
            reason: reason.clone(),
            at: Utc::now(),
        });
        self.progress.emit(WorkflowProgress::Terminal {
            status,
            failed_step,
            reason,
        });
        status.into()
    }

    /// Append a run record (best-effort: a failed write is logged, not fatal — a
    /// lost record only degrades crash-surfacing, never the running workflow).
    fn record(&self, record: &RunRecord) {
        if let Err(e) = switchboard_core::append_jsonl(&self.run_path, record) {
            tracing::warn!(path = %self.run_path.display(), error = %e, "failed to append workflow run record");
        }
    }
}

// Takes the error by value because it is used as a `map_err` fn, which hands the
// owned error in.
#[allow(clippy::needless_pass_by_value)]
fn render_failed(err: switchboard_workflow::WorkflowError) -> StepError {
    StepError::Failed(err.to_string())
}

#[cfg(test)]
mod tests {
    //! Integration tests through the **real `Dispatcher` + `MockHarnessAdapter`**
    //! (the `dispatcher_with_mock.rs` model): the interpreter drives genuine
    //! concurrency, not a mock of it. A per-agent `MockFactory` vends the agent's
    //! next scenario; a shared `RecordingEmitter` lets tests observe turns, and a
    //! recording journal captures each dispatched body for composition assertions.

    use super::*;
    use std::collections::{HashMap, VecDeque};
    use std::sync::Mutex;
    use switchboard_core::{AgentRecord, Attachment, HarnessKind, SendId, SessionLocator};
    use switchboard_dispatcher::{
        ConversationJournal, DispatchContext, JournalError, NoopMetadataCache,
        NoopSessionLocatorSink, RecordingEmitter,
    };
    use switchboard_harness::mock::{MockHarnessAdapter, MockScenario};
    use switchboard_harness::{DispatchOptions, TurnId};
    use switchboard_prompts::{InMemorySecretStore, PromptService};
    use switchboard_workflow::{InputValue, bind_invocation, parse_workflow};

    /// Captures each dispatched body (the user-side prompt) per agent, so tests can
    /// assert what the interpreter composed (forward blocks, rendered prompts).
    struct RecordingJournal {
        sends: Arc<Mutex<Vec<(AgentId, String)>>>,
    }
    impl ConversationJournal for RecordingJournal {
        fn record_send(
            &self,
            _turn_id: TurnId,
            agent_id: AgentId,
            prompt: &str,
            _attachments: &[Attachment],
            _at: chrono::DateTime<Utc>,
        ) -> Result<(), JournalError> {
            self.sends
                .lock()
                .unwrap()
                .push((agent_id, prompt.to_owned()));
            Ok(())
        }
        fn record_outcome(
            &self,
            _: TurnId,
            _: AgentId,
            _: &TurnOutcome,
            _: chrono::DateTime<Utc>,
            _: chrono::DateTime<Utc>,
        ) {
        }
    }

    /// Per-agent factory: vends the agent's next mock scenario each turn (the actor
    /// holds the factory it was spawned with, so the scenario queue persists across
    /// that agent's turns).
    struct MockFactory {
        agent: AgentRecord,
        scenarios: Mutex<VecDeque<MockScenario>>,
        emitter: Arc<RecordingEmitter>,
        journal: Arc<dyn ConversationJournal>,
    }
    impl DispatchContextFactory for MockFactory {
        fn build(&self, _send_id: SendId) -> DispatchContext {
            let scenario = self
                .scenarios
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(MockScenario::Streaming);
            DispatchContext {
                adapter: Arc::new(MockHarnessAdapter::with_scenario(scenario)),
                cwd: PathBuf::from("."),
                agent: self.agent.clone(),
                emitter: Arc::clone(&self.emitter) as Arc<dyn switchboard_dispatcher::EventEmitter>,
                options: DispatchOptions::default(),
                journal: Arc::clone(&self.journal),
                metadata: Arc::new(NoopMetadataCache),
                locator_sink: Arc::new(NoopSessionLocatorSink),
            }
        }
        fn idle_emitter(&self) -> Arc<dyn switchboard_dispatcher::EventEmitter> {
            Arc::clone(&self.emitter) as Arc<dyn switchboard_dispatcher::EventEmitter>
        }
    }

    struct Provider {
        factories: HashMap<AgentId, Arc<MockFactory>>,
    }
    impl DispatchFactoryProvider for Provider {
        fn factory_for(&self, agent_id: AgentId) -> Option<Arc<dyn DispatchContextFactory>> {
            self.factories
                .get(&agent_id)
                .map(|f| Arc::clone(f) as Arc<dyn DispatchContextFactory>)
        }
    }

    /// Test rig: a set of named agents with per-agent scenario queues, one shared
    /// dispatcher / emitter / recording journal.
    struct Rig {
        dispatcher: Arc<Dispatcher>,
        provider: Arc<Provider>,
        agents: BTreeMap<String, AgentId>, // canonical name → id
        names: Vec<String>,                // display names (the roster)
        ids: HashMap<String, AgentId>,     // display name → id
        emitter: Arc<RecordingEmitter>,
        sends: Arc<Mutex<Vec<(AgentId, String)>>>,
    }

    fn rig(agents: Vec<(&str, Vec<MockScenario>)>) -> Rig {
        let emitter = Arc::new(RecordingEmitter::new());
        let sends = Arc::new(Mutex::new(Vec::new()));
        let journal: Arc<dyn ConversationJournal> = Arc::new(RecordingJournal {
            sends: Arc::clone(&sends),
        });
        let mut factories = HashMap::new();
        let mut agent_map = BTreeMap::new();
        let mut ids = HashMap::new();
        let mut names = Vec::new();
        // `MockScenario` is not `Clone` (some variants hold a `Notify`), so the
        // scenario queues are moved in, not copied.
        for (name, scenarios) in agents {
            let record = AgentRecord {
                id: Uuid::now_v7(),
                project_id: Uuid::now_v7(),
                name: name.to_owned(),
                harness: HarnessKind::ClaudeCode,
                session_locator: Some(SessionLocator::Uuid(Uuid::now_v7())),
                model: None,
                effort: None,
                created_at: Utc::now(),
            };
            let id = record.id;
            factories.insert(
                id,
                Arc::new(MockFactory {
                    agent: record,
                    scenarios: Mutex::new(VecDeque::from(scenarios)),
                    emitter: Arc::clone(&emitter),
                    journal: Arc::clone(&journal),
                }),
            );
            agent_map.insert(canonicalize_for_uniqueness(name), id);
            ids.insert(name.to_owned(), id);
            names.push(name.to_owned());
        }
        Rig {
            dispatcher: Arc::new(Dispatcher::new()),
            provider: Arc::new(Provider { factories }),
            agents: agent_map,
            names,
            ids,
            emitter,
            sends,
        }
    }

    fn supplied(pairs: Vec<(&str, InputValue)>) -> BTreeMap<String, InputValue> {
        pairs.into_iter().map(|(k, v)| (k.to_owned(), v)).collect()
    }

    fn text(s: &str) -> InputValue {
        InputValue::Text(s.to_owned())
    }

    fn list(items: &[&str]) -> InputValue {
        InputValue::List(items.iter().map(|s| (*s).to_owned()).collect())
    }

    /// Build a `WorkflowRun` for `yaml` + `supplied` inputs against `rig`, writing
    /// run records under a fresh temp dir (kept alive by the returned guard). Uses
    /// a null progress sink; use [`build_run_with_sink`] to observe progress.
    #[allow(clippy::needless_pass_by_value)] // owned args read more cleanly at call sites
    fn build_run(
        rig: &Rig,
        prompts: PromptService,
        yaml: &str,
        stem: &str,
        supplied: BTreeMap<String, InputValue>,
        cancel: CancellationToken,
    ) -> (WorkflowRun, tempfile::TempDir, PathBuf) {
        build_run_with_sink(
            rig,
            prompts,
            yaml,
            stem,
            supplied,
            cancel,
            Arc::new(NullProgressSink),
        )
    }

    /// Like [`build_run`] but with a caller-supplied progress sink, so the
    /// progress-seam test can record the emitted events.
    #[allow(clippy::needless_pass_by_value)]
    fn build_run_with_sink(
        rig: &Rig,
        prompts: PromptService,
        yaml: &str,
        stem: &str,
        supplied: BTreeMap<String, InputValue>,
        cancel: CancellationToken,
        progress: Arc<dyn ProgressSink>,
    ) -> (WorkflowRun, tempfile::TempDir, PathBuf) {
        let workflow = parse_workflow(stem, yaml).expect("workflow parses");
        let inputs = bind_invocation(&workflow, &supplied, &rig.names).expect("invocation binds");
        // Mirror invoke: a declared `text` input can feed a prompt arg of the same
        // name. These interpreter tests bind every prompt arg via template_vars, so
        // the text-input passthrough is enough; derived-arg assembly is covered by
        // the dedicated isolation test below.
        let user_args = inputs
            .iter()
            .filter_map(|(k, v)| match v {
                ScopeValue::Text(s) => Some((k.clone(), s.clone())),
                ScopeValue::List(_) => None,
            })
            .collect();
        let dir = tempfile::tempdir().unwrap();
        let run_path = dir.path().join("run.jsonl");
        let run = WorkflowRun {
            workflow,
            inputs,
            user_args,
            agents: rig.agents.clone(),
            dispatcher: Arc::clone(&rig.dispatcher),
            prompts,
            factories: Arc::clone(&rig.provider) as Arc<dyn DispatchFactoryProvider>,
            run_path: run_path.clone(),
            progress,
            cancel,
        };
        (run, dir, run_path)
    }

    /// Build a run with **explicit** user-fillable prompt-arg values, to exercise
    /// M3's per-prompt argument assembly (a derived arg the user filled is not a
    /// declared input, so `build_run` can't inject it from the bound scope).
    #[allow(clippy::needless_pass_by_value)]
    fn build_run_with_user_args(
        rig: &Rig,
        prompts: PromptService,
        yaml: &str,
        stem: &str,
        supplied: BTreeMap<String, InputValue>,
        user_args: BTreeMap<String, String>,
    ) -> (WorkflowRun, tempfile::TempDir, PathBuf) {
        let workflow = parse_workflow(stem, yaml).expect("workflow parses");
        let inputs = bind_invocation(&workflow, &supplied, &rig.names).expect("invocation binds");
        let dir = tempfile::tempdir().unwrap();
        let run_path = dir.path().join("run.jsonl");
        let run = WorkflowRun {
            workflow,
            inputs,
            user_args,
            agents: rig.agents.clone(),
            dispatcher: Arc::clone(&rig.dispatcher),
            prompts,
            factories: Arc::clone(&rig.provider) as Arc<dyn DispatchFactoryProvider>,
            run_path: run_path.clone(),
            progress: Arc::new(NullProgressSink),
            cancel: CancellationToken::new(),
        };
        (run, dir, run_path)
    }

    fn user_args(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    /// A progress sink that records every event for assertions.
    #[derive(Default)]
    struct RecordingProgressSink {
        events: std::sync::Mutex<Vec<WorkflowProgress>>,
    }

    impl RecordingProgressSink {
        fn snapshot(&self) -> Vec<WorkflowProgress> {
            self.events.lock().unwrap().clone()
        }
    }

    impl ProgressSink for RecordingProgressSink {
        fn emit(&self, event: WorkflowProgress) {
            self.events.lock().unwrap().push(event);
        }
    }

    fn records(path: &std::path::Path) -> Vec<RunRecord> {
        switchboard_core::read_jsonl(path).unwrap()
    }

    fn prompt_service_with(files: &[(&str, &str)]) -> (tempfile::TempDir, PromptService) {
        let dir = tempfile::tempdir().unwrap();
        let prompts_dir = dir.path().join("prompts");
        std::fs::create_dir(&prompts_dir).unwrap();
        for (name, body) in files {
            std::fs::write(prompts_dir.join(format!("{name}.md")), body).unwrap();
        }
        let service = PromptService::new(
            dir.path().join("config.yaml"),
            prompts_dir,
            None,
            Arc::new(InMemorySecretStore::new()),
        );
        (dir, service)
    }

    const SEQUENTIAL: &str = "name: seq\ndescription: d\ninputs:\n  planner: agent\n  implementer: agent\n  goal: text\nsteps:\n  - label: s\n    send:\n      to: \"{{ planner }}\"\n      text: \"Plan: {{ goal }}\"\n  - label: s\n    wait_for:\n      agent: \"{{ planner }}\"\n  - label: s\n    send:\n      to: \"{{ implementer }}\"\n      forward_from: \"{{ planner }}\"\n      text: \"Execute the plan above.\"\n  - label: s\n    wait_for:\n      agent: \"{{ implementer }}\"\n";

    #[tokio::test]
    async fn sequential_handoff_runs_to_complete() {
        let rig = rig(vec![
            ("planner", vec![MockScenario::Streaming]),
            ("implementer", vec![MockScenario::Streaming]),
        ]);
        let (run, _dir, path) = build_run(
            &rig,
            PromptService::disabled(),
            SEQUENTIAL,
            "seq",
            supplied(vec![
                ("planner", text("planner")),
                ("implementer", text("implementer")),
                ("goal", text("ship it")),
            ]),
            CancellationToken::new(),
        );
        assert_eq!(run.execute().await, RunStatus::Complete);

        let recs = records(&path);
        assert!(matches!(
            recs.first(),
            Some(RunRecord::Started { total_steps: 4, .. })
        ));
        assert!(matches!(
            recs.last(),
            Some(RunRecord::Terminal {
                status: TerminalStatus::Complete,
                ..
            })
        ));
        // The implementer's dispatched body forwards the planner's output verbatim.
        let implementer = rig.ids["implementer"];
        let body = rig
            .sends
            .lock()
            .unwrap()
            .iter()
            .find(|(id, _)| *id == implementer)
            .map(|(_, b)| b.clone())
            .expect("implementer was dispatched");
        assert!(
            body.contains("=== START forwarded from planner ==="),
            "got: {body}"
        );
        assert!(body.contains("Execute the plan above."));
    }

    #[tokio::test]
    async fn workflow_send_emits_a_user_message_per_recipient_sharing_send_id() {
        // Each `send` surfaces its dispatched body as a live user message on every
        // recipient's channel; a fan-out's recipients share one send_id so the UI
        // groups them into one user row + per-recipient columns (matching a manual
        // send and the reloaded journal view). Emitted only on acceptance.
        let rig = rig(vec![
            ("rev-1", vec![MockScenario::Streaming]),
            ("rev-2", vec![MockScenario::Streaming]),
        ]);
        let yaml = "name: fan\ndescription: d\ninputs:\n  revs: [agent]\nsteps:\n  - label: s\n    send:\n      to: \"{{ revs }}\"\n      text: \"please review\"\n";
        let (run, _dir, _path) = build_run(
            &rig,
            PromptService::disabled(),
            yaml,
            "fan",
            supplied(vec![("revs", list(&["rev-1", "rev-2"]))]),
            CancellationToken::new(),
        );
        assert_eq!(run.execute().await, RunStatus::Complete);

        let user_messages: Vec<(String, serde_json::Value)> = rig
            .emitter
            .snapshot()
            .into_iter()
            .filter(|(_, payload)| payload["type"] == "user_message")
            .collect();
        assert_eq!(user_messages.len(), 2, "one per recipient");
        assert!(
            user_messages
                .iter()
                .all(|(_, p)| p["text"] == "please review")
        );
        let send_ids: std::collections::HashSet<String> = user_messages
            .iter()
            .map(|(_, p)| p["send_id"].to_string())
            .collect();
        assert_eq!(
            send_ids.len(),
            1,
            "a fan-out's recipients share one send_id"
        );
        let channels: std::collections::HashSet<String> =
            user_messages.iter().map(|(c, _)| c.clone()).collect();
        assert!(channels.contains(&format!("agent:{}", rig.ids["rev-1"])));
        assert!(channels.contains(&format!("agent:{}", rig.ids["rev-2"])));
    }

    #[tokio::test]
    async fn fan_in_aggregates_reviewer_outputs_into_the_prompt() {
        let (_pd, prompts) = prompt_service_with(&[
            (
                "review",
                "---\nname: review\ndescription: d\narguments:\n  - name: context\n---\nReview. {{ context }}\n",
            ),
            (
                "aggregate",
                "---\nname: aggregate\ndescription: d\narguments:\n  - name: feedback\n---\nSummarize:\n{{ feedback }}\n",
            ),
        ]);
        let rig = rig(vec![
            ("primary", vec![MockScenario::Streaming]),
            ("reviewer-1", vec![MockScenario::Streaming]),
            ("reviewer-2", vec![MockScenario::Streaming]),
        ]);
        let yaml = "name: fan\ndescription: d\ninputs:\n  primary_agent: agent\n  reviewer_agents: [agent]\n  user_context: text?\nsteps:\n  - label: s\n    send:\n      to: \"{{ reviewer_agents }}\"\n      prompt: \"local:review\"\n      template_vars:\n        context: \"{{ user_context }}\"\n  - label: s\n    wait_for_all:\n      agents: \"{{ reviewer_agents }}\"\n  - label: s\n    send:\n      to: \"{{ primary_agent }}\"\n      prompt: \"local:aggregate\"\n      template_vars:\n        feedback: \"{{ aggregated_responses(reviewer_agents) }}\"\n";
        let (run, _dir, path) = build_run(
            &rig,
            prompts,
            yaml,
            "fan",
            supplied(vec![
                ("primary_agent", text("primary")),
                ("reviewer_agents", list(&["reviewer-1", "reviewer-2"])),
            ]),
            CancellationToken::new(),
        );
        assert_eq!(run.execute().await, RunStatus::Complete);
        assert!(matches!(
            records(&path).last(),
            Some(RunRecord::Terminal {
                status: TerminalStatus::Complete,
                ..
            })
        ));

        let primary = rig.ids["primary"];
        let body = rig
            .sends
            .lock()
            .unwrap()
            .iter()
            .find(|(id, _)| *id == primary)
            .map(|(_, b)| b.clone())
            .expect("primary dispatched");
        // The aggregation prompt rendered with both reviewers' outputs, in order,
        // each in a canonical response block.
        assert!(body.contains("Summarize:"));
        assert!(
            body.contains("=== START response from reviewer-1 ==="),
            "got: {body}"
        );
        assert!(body.contains("=== START response from reviewer-2 ==="));
        assert!(body.find("reviewer-1").unwrap() < body.find("reviewer-2").unwrap());
    }

    #[tokio::test]
    async fn diamond_two_heterogeneous_sends_fan_in_to_one() {
        // The "diamond" shape: two *different* sends — distinct messages/jobs (a
        // security review and a code review) — issued back-to-back with no wait
        // between, then a barrier fans both into one worker. A single fan-out
        // `send` (one message to a list) cannot express this; the decoupled
        // `wait_for` is what makes it possible. This test asserts the *shape* and
        // composed fan-in body; genuine overlap is proven separately by
        // `heterogeneous_fan_out_runs_concurrently_not_serially`.
        let rig = rig(vec![
            ("sec", vec![MockScenario::Streaming]),
            ("code", vec![MockScenario::Streaming]),
            ("worker", vec![MockScenario::Streaming]),
        ]);
        let yaml = "name: diamond\ndescription: d\ninputs:\n  sec: agent\n  code: agent\n  worker: agent\nsteps:\n  - label: Security review\n    send:\n      to: \"{{ sec }}\"\n      text: \"Security review the changes.\"\n  - label: Code review\n    send:\n      to: \"{{ code }}\"\n      text: \"Code review the changes.\"\n  - label: Wait for security review\n    wait_for:\n      agent: \"{{ sec }}\"\n  - label: Wait for code review\n    wait_for:\n      agent: \"{{ code }}\"\n  - label: Combine reviews\n    send:\n      to: \"{{ worker }}\"\n      text: \"Combine these reviews: {{ aggregated_responses([sec, code]) }}\"\n";
        let (run, _dir, path) = build_run(
            &rig,
            PromptService::disabled(),
            yaml,
            "diamond",
            supplied(vec![
                ("sec", text("sec")),
                ("code", text("code")),
                ("worker", text("worker")),
            ]),
            CancellationToken::new(),
        );
        assert_eq!(run.execute().await, RunStatus::Complete);
        assert!(matches!(
            records(&path).last(),
            Some(RunRecord::Terminal {
                status: TerminalStatus::Complete,
                ..
            })
        ));

        let sends = rig.sends.lock().unwrap();
        let body_for = |id: AgentId| -> String {
            sends
                .iter()
                .find(|(i, _)| *i == id)
                .map(|(_, b)| b.clone())
                .expect("dispatched")
        };
        // Each branch got its own distinct message (true heterogeneity, not a
        // single shared fan-out message).
        assert!(body_for(rig.ids["sec"]).contains("Security review"));
        assert!(body_for(rig.ids["code"]).contains("Code review"));
        // The worker's body fans both branches in, in declared order.
        let worker_body = body_for(rig.ids["worker"]);
        assert!(
            worker_body.contains("=== START response from sec ==="),
            "got: {worker_body}"
        );
        assert!(
            worker_body.contains("=== START response from code ==="),
            "got: {worker_body}"
        );
        assert!(worker_body.find("from sec").unwrap() < worker_body.find("from code").unwrap());
    }

    #[tokio::test]
    async fn fan_out_one_to_two_then_collapse_to_one() {
        // One → two → one: a planner's output fans out to two *different* workers
        // (distinct messages) issued back-to-back with no wait between their sends,
        // then both collapse into a reviewer via multi-source `forward_from`.
        // Asserts the shape and composed bodies; overlap itself is proven by
        // `heterogeneous_fan_out_runs_concurrently_not_serially`.
        let rig = rig(vec![
            ("planner", vec![MockScenario::Streaming]),
            ("impl_a", vec![MockScenario::Streaming]),
            ("impl_b", vec![MockScenario::Streaming]),
            ("reviewer", vec![MockScenario::Streaming]),
        ]);
        let yaml = "name: fanout\ndescription: d\ninputs:\n  planner: agent\n  impl_a: agent\n  impl_b: agent\n  reviewer: agent\n  goal: text\nsteps:\n  - label: Plan\n    send:\n      to: \"{{ planner }}\"\n      text: \"Plan: {{ goal }}\"\n  - label: Wait for plan\n    wait_for:\n      agent: \"{{ planner }}\"\n  - label: Implement part A\n    send:\n      to: \"{{ impl_a }}\"\n      forward_from: \"{{ planner }}\"\n      text: \"Implement part A of the plan above.\"\n  - label: Implement part B\n    send:\n      to: \"{{ impl_b }}\"\n      forward_from: \"{{ planner }}\"\n      text: \"Implement part B of the plan above.\"\n  - label: Wait for A\n    wait_for:\n      agent: \"{{ impl_a }}\"\n  - label: Wait for B\n    wait_for:\n      agent: \"{{ impl_b }}\"\n  - label: Review both\n    send:\n      to: \"{{ reviewer }}\"\n      forward_from: [\"{{ impl_a }}\", \"{{ impl_b }}\"]\n      text: \"Review both implementations.\"\n";
        let (run, _dir, path) = build_run(
            &rig,
            PromptService::disabled(),
            yaml,
            "fanout",
            supplied(vec![
                ("planner", text("planner")),
                ("impl_a", text("impl_a")),
                ("impl_b", text("impl_b")),
                ("reviewer", text("reviewer")),
                ("goal", text("ship it")),
            ]),
            CancellationToken::new(),
        );
        assert_eq!(run.execute().await, RunStatus::Complete);
        assert!(matches!(
            records(&path).last(),
            Some(RunRecord::Terminal {
                status: TerminalStatus::Complete,
                ..
            })
        ));

        let sends = rig.sends.lock().unwrap();
        let body_for = |id: AgentId| -> String {
            sends
                .iter()
                .find(|(i, _)| *i == id)
                .map(|(_, b)| b.clone())
                .expect("dispatched")
        };
        // Both implementers received the planner's output (the fan-out).
        assert!(body_for(rig.ids["impl_a"]).contains("=== START forwarded from planner ==="));
        assert!(body_for(rig.ids["impl_b"]).contains("=== START forwarded from planner ==="));
        // The reviewer received both implementers' outputs (the collapse).
        let reviewer_body = body_for(rig.ids["reviewer"]);
        assert!(
            reviewer_body.contains("=== START forwarded from impl_a ==="),
            "got: {reviewer_body}"
        );
        assert!(
            reviewer_body.contains("=== START forwarded from impl_b ==="),
            "got: {reviewer_body}"
        );
    }

    #[tokio::test]
    async fn heterogeneous_fan_out_runs_concurrently_not_serially() {
        // Proves the overlap is real, not just expressible: two different sends with
        // no wait between them put *both* agents in flight at once. If `send`
        // blocked on its recipient, the second turn could not start until the first
        // finished — and `wait_for_type("turn_start", 2)` would hang.
        let sig_a = Arc::new(tokio::sync::Notify::new());
        let sig_b = Arc::new(tokio::sync::Notify::new());
        let rig = rig(vec![
            (
                "a",
                vec![MockScenario::CompletesOnSignal(Arc::clone(&sig_a))],
            ),
            (
                "b",
                vec![MockScenario::CompletesOnSignal(Arc::clone(&sig_b))],
            ),
        ]);
        let yaml = "name: conc\ndescription: d\ninputs:\n  a: agent\n  b: agent\nsteps:\n  - label: Send a\n    send:\n      to: \"{{ a }}\"\n      text: a\n  - label: Send b\n    send:\n      to: \"{{ b }}\"\n      text: b\n  - label: Wait a\n    wait_for:\n      agent: \"{{ a }}\"\n  - label: Wait b\n    wait_for:\n      agent: \"{{ b }}\"\n";
        let (run, _dir, _path) = build_run(
            &rig,
            PromptService::disabled(),
            yaml,
            "conc",
            supplied(vec![("a", text("a")), ("b", text("b"))]),
            CancellationToken::new(),
        );
        let handle = tokio::spawn(run.execute());
        // Both turns are in flight before either completes — the overlap a
        // block-after-send model could never produce.
        rig.emitter.wait_for_type("turn_start", 2).await;
        // Release both; the barriers resolve and the run completes.
        sig_a.notify_one();
        sig_b.notify_one();
        assert_eq!(handle.await.unwrap(), RunStatus::Complete);
    }

    #[tokio::test]
    async fn review_and_recommend_builtin_runs_end_to_end() {
        // Runs the *actual shipped* built-in workflow YAML through the executor
        // with mock agents — guarding the file users invoke, not a copy. Parse-only
        // coverage can't catch a broken helper, a renamed prompt argument, or a bad
        // agent reference; this can.
        let rig = rig(vec![
            ("rev-1", vec![MockScenario::Streaming]),
            ("rev-2", vec![MockScenario::Streaming]),
            ("boss", vec![MockScenario::Streaming]),
        ]);
        let yaml = switchboard_workflow::builtin_workflow_content("review-and-recommend")
            .expect("shipped built-in");
        // A builtins-enabled `PromptService` (the default) so `builtin:code-review`
        // resolves; no local prompts needed.
        let (_pd, prompts) = prompt_service_with(&[]);
        let (run, _dir, path) = build_run(
            &rig,
            prompts,
            yaml,
            "review-and-recommend",
            supplied(vec![
                ("reviewers", list(&["rev-1", "rev-2"])),
                ("worker", text("boss")),
            ]),
            CancellationToken::new(),
        );
        assert_eq!(run.execute().await, RunStatus::Complete);
        assert!(matches!(
            records(&path).last(),
            Some(RunRecord::Terminal {
                status: TerminalStatus::Complete,
                ..
            })
        ));

        let sends = rig.sends.lock().unwrap();
        let body_for = |id: AgentId| -> String {
            sends
                .iter()
                .find(|(i, _)| *i == id)
                .map(|(_, b)| b.clone())
                .expect("dispatched")
        };
        // Reviewers received the rendered `builtin:code-review` prompt.
        assert!(body_for(rig.ids["rev-1"]).contains("Code Review Guidelines"));
        // The worker received both reviewers' outputs aggregated into the handoff.
        let worker_body = body_for(rig.ids["boss"]);
        assert!(worker_body.contains("Here's feedback from several reviewers:"));
        assert!(
            worker_body.contains("=== START response from rev-1 ==="),
            "got: {worker_body}"
        );
        assert!(worker_body.contains("=== START response from rev-2 ==="));
    }

    #[tokio::test]
    async fn review_and_reconcile_builtin_runs_end_to_end() {
        // The flagship 8-step built-in, end to end. Today it is only parse-checked,
        // so reaching `Complete` is itself high-value; we also thread-check the
        // multi-round handoffs (reviewers and worker are each dispatched twice).
        let rig = rig(vec![
            (
                "rev-1",
                vec![MockScenario::Streaming, MockScenario::Streaming],
            ),
            (
                "rev-2",
                vec![MockScenario::Streaming, MockScenario::Streaming],
            ),
            (
                "boss",
                vec![MockScenario::Streaming, MockScenario::Streaming],
            ),
        ]);
        let yaml = switchboard_workflow::builtin_workflow_content("review-and-reconcile")
            .expect("shipped built-in");
        let (_pd, prompts) = prompt_service_with(&[]);
        let (run, _dir, path) = build_run(
            &rig,
            prompts,
            yaml,
            "review-and-reconcile",
            supplied(vec![
                ("reviewers", list(&["rev-1", "rev-2"])),
                ("worker", text("boss")),
            ]),
            CancellationToken::new(),
        );
        assert_eq!(run.execute().await, RunStatus::Complete);
        assert!(matches!(
            records(&path).last(),
            Some(RunRecord::Terminal {
                status: TerminalStatus::Complete,
                ..
            })
        ));

        let sends = rig.sends.lock().unwrap();
        let bodies_for = |id: AgentId| -> Vec<String> {
            sends
                .iter()
                .filter(|(i, _)| *i == id)
                .map(|(_, b)| b.clone())
                .collect()
        };
        // Worker dispatched twice: first the analysis (`builtin:analyze-ai-reviews`
        // fed the aggregated reviews), then the final-call handoff.
        let worker_bodies = bodies_for(rig.ids["boss"]);
        assert_eq!(worker_bodies.len(), 2, "worker dispatched twice");
        assert!(
            worker_bodies[0].contains("=== START response from rev-1 ==="),
            "got: {}",
            worker_bodies[0]
        );
        assert!(worker_bodies[1].contains("give your final recommendation"));
        // Reviewers dispatched twice: the code review, then weighing in on the
        // worker's verdict (which forwards `last_output(worker)`).
        let rev1_bodies = bodies_for(rig.ids["rev-1"]);
        assert_eq!(rev1_bodies.len(), 2, "reviewer dispatched twice");
        assert!(rev1_bodies[0].contains("Code Review Guidelines"));
        assert!(rev1_bodies[1].contains("An analyst reviewed all of the feedback"));
    }

    #[tokio::test]
    async fn user_fillable_arg_renders_into_the_prompt_or_omits_when_blank() {
        let body_for = |user: BTreeMap<String, String>| async move {
            let (_pd, prompts) = prompt_service_with(&[(
                "rev",
                "---\nname: rev\ndescription: d\narguments:\n  - name: context\n    required: false\n---\nReview.{% if context %} Context: {{ context }}{% endif %}\n",
            )]);
            let rig = rig(vec![("worker", vec![MockScenario::Streaming])]);
            let yaml = "name: w\ndescription: d\ninputs:\n  w: agent\nsteps:\n  - label: s\n    send:\n      to: \"{{ w }}\"\n      prompt: \"local:rev\"\n";
            let (run, _dir, _path) = build_run_with_user_args(
                &rig,
                prompts,
                yaml,
                "w",
                supplied(vec![("w", text("worker"))]),
                user,
            );
            assert_eq!(run.execute().await, RunStatus::Complete);
            let worker = rig.ids["worker"];
            rig.sends
                .lock()
                .unwrap()
                .iter()
                .find(|(id, _)| *id == worker)
                .map(|(_, b)| b.clone())
                .expect("worker dispatched")
        };

        // The user filled `context` (an auto-derived arg) → it renders in.
        let with = body_for(user_args(&[("context", "watch the error paths")])).await;
        assert!(
            with.contains("Context: watch the error paths"),
            "got: {with}"
        );

        // Unfilled → the arg is omitted, so the optional `{% if %}` takes its empty
        // branch (rather than receiving an empty string and rendering a stray label).
        let without = body_for(BTreeMap::new()).await;
        assert!(without.contains("Review."));
        assert!(!without.contains("Context:"), "got: {without}");
    }

    #[tokio::test]
    async fn each_prompt_receives_only_its_own_declared_args() {
        // Two prompts with disjoint args. The run carries both user values, but each
        // send must pass only the arg its own prompt declares — `render_template`
        // rejects unknown args, so a leak would fail the run.
        let (_pd, prompts) = prompt_service_with(&[
            (
                "pa",
                "---\nname: pa\ndescription: d\narguments:\n  - name: foo\n    required: true\n---\nA:{{ foo }}\n",
            ),
            (
                "pb",
                "---\nname: pb\ndescription: d\narguments:\n  - name: bar\n    required: true\n---\nB:{{ bar }}\n",
            ),
        ]);
        let rig = rig(vec![
            ("a", vec![MockScenario::Streaming]),
            ("b", vec![MockScenario::Streaming]),
        ]);
        let yaml = "name: iso\ndescription: d\ninputs:\n  a: agent\n  b: agent\nsteps:\n  - label: s\n    send:\n      to: \"{{ a }}\"\n      prompt: \"local:pa\"\n  - label: s\n    send:\n      to: \"{{ b }}\"\n      prompt: \"local:pb\"\n";
        let (run, _dir, _path) = build_run_with_user_args(
            &rig,
            prompts,
            yaml,
            "iso",
            supplied(vec![("a", text("a")), ("b", text("b"))]),
            user_args(&[("foo", "FOO"), ("bar", "BAR")]),
        );
        // Neither send fails on an unknown argument.
        assert_eq!(run.execute().await, RunStatus::Complete);

        let sends = rig.sends.lock().unwrap();
        let body_for = |id: AgentId| {
            sends
                .iter()
                .find(|(sid, _)| *sid == id)
                .map(|(_, b)| b.clone())
                .expect("dispatched")
        };
        let a_body = body_for(rig.ids["a"]);
        let b_body = body_for(rig.ids["b"]);
        assert!(
            a_body.contains("A:FOO"),
            "prompt A got its own arg: {a_body}"
        );
        assert!(
            !a_body.contains("BAR"),
            "prompt A must not see B's arg: {a_body}"
        );
        assert!(
            b_body.contains("B:BAR"),
            "prompt B got its own arg: {b_body}"
        );
        assert!(
            !b_body.contains("FOO"),
            "prompt B must not see A's arg: {b_body}"
        );
    }

    #[tokio::test]
    async fn contention_fails_the_step() {
        // Pre-occupy the agent with a held turn (from "another source"), so the
        // workflow's FailFast send to it is refused → step failure.
        let signal = Arc::new(tokio::sync::Notify::new());
        let rig = rig(vec![(
            "worker",
            vec![MockScenario::CompletesOnSignal(Arc::clone(&signal))],
        )]);
        let worker = rig.ids["worker"];
        let _busy = rig
            .dispatcher
            .send_message_awaiting_completion(
                worker,
                "occupy",
                Vec::new(),
                Uuid::now_v7(),
                rig.provider.factory_for(worker).unwrap(),
            )
            .await;
        rig.emitter.wait_for_type("turn_start", 1).await;

        let yaml = "name: c\ndescription: d\ninputs:\n  w: agent\nsteps:\n  - label: s\n    send:\n      to: \"{{ w }}\"\n      text: hi\n";
        let (run, _dir, path) = build_run(
            &rig,
            PromptService::disabled(),
            yaml,
            "c",
            supplied(vec![("w", text("worker"))]),
            CancellationToken::new(),
        );
        assert_eq!(run.execute().await, RunStatus::Failed);
        assert!(matches!(
            records(&path).last(),
            Some(RunRecord::Terminal {
                status: TerminalStatus::Failed,
                failed_step: Some(0),
                reason: Some(_),
                ..
            })
        ));
        signal.notify_one();
    }

    #[tokio::test]
    async fn sibling_failure_fails_run_and_keeps_survivor() {
        let rig = rig(vec![
            ("good", vec![MockScenario::Streaming]),
            ("bad", vec![MockScenario::DispatchFails]),
        ]);
        let yaml = "name: s\ndescription: d\ninputs:\n  rs: [agent]\nsteps:\n  - label: s\n    send:\n      to: \"{{ rs }}\"\n      text: review\n  - label: s\n    wait_for_all:\n      agents: \"{{ rs }}\"\n";
        let (run, _dir, path) = build_run(
            &rig,
            PromptService::disabled(),
            yaml,
            "s",
            supplied(vec![("rs", list(&["good", "bad"]))]),
            CancellationToken::new(),
        );
        assert_eq!(run.execute().await, RunStatus::Failed);
        assert!(matches!(
            records(&path).last(),
            Some(RunRecord::Terminal {
                status: TerminalStatus::Failed,
                ..
            })
        ));
        // The surviving sibling ran to a completed terminal and was not cancelled.
        let good = rig.ids["good"];
        let good_channel = format!("agent:{good}");
        let completed = rig.emitter.snapshot().into_iter().any(|(ch, p)| {
            ch == good_channel
                && p.get("type").and_then(|t| t.as_str()) == Some("turn_end")
                && p.get("outcome")
                    .and_then(|o| o.get("status"))
                    .and_then(|s| s.as_str())
                    == Some("completed")
        });
        assert!(
            completed,
            "the surviving sibling must complete, not be cancelled"
        );
    }

    #[tokio::test]
    async fn workflow_cancel_marks_run_cancelled() {
        let rig = rig(vec![("w", vec![MockScenario::AwaitCancellation])]);
        let cancel = CancellationToken::new();
        let yaml = "name: x\ndescription: d\ninputs:\n  w: agent\nsteps:\n  - label: s\n    send:\n      to: \"{{ w }}\"\n      text: go\n  - label: s\n    wait_for:\n      agent: \"{{ w }}\"\n";
        let (run, _dir, path) = build_run(
            &rig,
            PromptService::disabled(),
            yaml,
            "x",
            supplied(vec![("w", text("w"))]),
            cancel.clone(),
        );
        let emitter = Arc::clone(&rig.emitter);
        let (status, ()) = tokio::join!(run.execute(), async move {
            // Cancel once the awaited turn is in flight.
            emitter.wait_for_type("turn_start", 1).await;
            cancel.cancel();
        });
        assert_eq!(status, RunStatus::Cancelled);
        assert!(matches!(
            records(&path).last(),
            Some(RunRecord::Terminal {
                status: TerminalStatus::Cancelled,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn participant_turn_cancelled_marks_run_cancelled() {
        // Distinct from a workflow-level cancel: a user cancels a *participating*
        // agent's turn (the awaited completion resolves `Cancelled`), which marks
        // the whole run cancelled — uniformly, per the spec.
        let rig = rig(vec![("w", vec![MockScenario::AwaitCancellation])]);
        let yaml = "name: x\ndescription: d\ninputs:\n  w: agent\nsteps:\n  - label: s\n    send:\n      to: \"{{ w }}\"\n      text: go\n  - label: s\n    wait_for:\n      agent: \"{{ w }}\"\n";
        let (run, _dir, path) = build_run(
            &rig,
            PromptService::disabled(),
            yaml,
            "x",
            supplied(vec![("w", text("w"))]),
            CancellationToken::new(),
        );
        let emitter = Arc::clone(&rig.emitter);
        let dispatcher = Arc::clone(&rig.dispatcher);
        let worker = rig.ids["w"];
        let (status, ()) = tokio::join!(run.execute(), async move {
            // Cancel the agent's turn directly (not the workflow token).
            emitter.wait_for_type("turn_start", 1).await;
            dispatcher.cancel(worker, CancelSource::User);
        });
        assert_eq!(status, RunStatus::Cancelled);
        assert!(matches!(
            records(&path).last(),
            Some(RunRecord::Terminal {
                status: TerminalStatus::Cancelled,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn trailing_settle_holds_for_fire_and_forget_then_completes() {
        // A send with no trailing wait_for: the run must hold open until it settles.
        let rig = rig(vec![("w", vec![MockScenario::Streaming])]);
        let yaml = "name: t\ndescription: d\ninputs:\n  w: agent\nsteps:\n  - label: s\n    send:\n      to: \"{{ w }}\"\n      text: go\n";
        let (run, _dir, path) = build_run(
            &rig,
            PromptService::disabled(),
            yaml,
            "t",
            supplied(vec![("w", text("w"))]),
            CancellationToken::new(),
        );
        assert_eq!(run.execute().await, RunStatus::Complete);
        assert!(matches!(
            records(&path).last(),
            Some(RunRecord::Terminal {
                status: TerminalStatus::Complete,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn trailing_failure_marks_run_failed() {
        let rig = rig(vec![("w", vec![MockScenario::DispatchFails])]);
        let yaml = "name: t\ndescription: d\ninputs:\n  w: agent\nsteps:\n  - label: s\n    send:\n      to: \"{{ w }}\"\n      text: go\n";
        let (run, _dir, path) = build_run(
            &rig,
            PromptService::disabled(),
            yaml,
            "t",
            supplied(vec![("w", text("w"))]),
            CancellationToken::new(),
        );
        assert_eq!(run.execute().await, RunStatus::Failed);
        assert!(matches!(
            records(&path).last(),
            Some(RunRecord::Terminal {
                status: TerminalStatus::Failed,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn forward_from_with_no_output_fails() {
        // forward_from references an agent that has no completed output this run
        // (never waited on) → step failure.
        let rig = rig(vec![
            ("a", vec![MockScenario::Streaming]),
            ("b", vec![MockScenario::Streaming]),
        ]);
        let yaml = "name: f\ndescription: d\ninputs:\n  a: agent\n  b: agent\nsteps:\n  - label: s\n    send:\n      to: \"{{ b }}\"\n      forward_from: \"{{ a }}\"\n      text: use it\n";
        let (run, _dir, path) = build_run(
            &rig,
            PromptService::disabled(),
            yaml,
            "f",
            supplied(vec![("a", text("a")), ("b", text("b"))]),
            CancellationToken::new(),
        );
        assert_eq!(run.execute().await, RunStatus::Failed);
        let last = records(&path).pop().unwrap();
        assert!(matches!(
            &last,
            RunRecord::Terminal { status: TerminalStatus::Failed, reason: Some(r), .. } if r.contains("no in-workflow completed output")
        ));
    }

    #[tokio::test]
    async fn capability_gate_rejects_for_each() {
        // `for_each` parses but is not executable in this version — the
        // interpreter fails clearly (defense-in-depth behind the invoke gate).
        let rig = rig(vec![("w", vec![MockScenario::Streaming])]);
        let yaml = "name: g\ndescription: d\ninputs:\n  ms: [text]\n  w: agent\nsteps:\n  - label: s\n    for_each:\n      item: m\n      in: \"{{ ms }}\"\n      steps:\n        - label: s\n          send:\n            to: \"{{ w }}\"\n            text: \"{{ m }}\"\n";
        let (run, _dir, path) = build_run(
            &rig,
            PromptService::disabled(),
            yaml,
            "g",
            supplied(vec![("ms", list(&["x"])), ("w", text("w"))]),
            CancellationToken::new(),
        );
        assert_eq!(run.execute().await, RunStatus::Failed);
        let last = records(&path).pop().unwrap();
        assert!(matches!(
            &last,
            RunRecord::Terminal { status: TerminalStatus::Failed, reason: Some(r), .. } if r.contains("not supported")
        ));
    }

    #[tokio::test]
    async fn progress_sink_receives_start_step_starts_and_terminal_in_order() {
        let rig = rig(vec![
            ("planner", vec![MockScenario::Streaming]),
            ("implementer", vec![MockScenario::Streaming]),
        ]);
        let sink = Arc::new(RecordingProgressSink::default());
        let (run, _dir, _path) = build_run_with_sink(
            &rig,
            PromptService::disabled(),
            SEQUENTIAL,
            "seq",
            supplied(vec![
                ("planner", text("planner")),
                ("implementer", text("implementer")),
                ("goal", text("ship it")),
            ]),
            CancellationToken::new(),
            sink.clone(),
        );
        assert_eq!(run.execute().await, RunStatus::Complete);

        // Start (name + total), one step-start per top-level step in order, then a
        // Complete terminal — and no agent output text anywhere in the stream.
        let events = sink.snapshot();
        assert_eq!(
            events.first(),
            Some(&WorkflowProgress::Started {
                workflow: "seq".to_owned(),
                total_steps: 4,
            })
        );
        let step_starts: Vec<usize> = events
            .iter()
            .filter_map(|e| match e {
                WorkflowProgress::StepStarted { step_index } => Some(*step_index),
                _ => None,
            })
            .collect();
        assert_eq!(step_starts, vec![0, 1, 2, 3]);
        assert_eq!(
            events.last(),
            Some(&WorkflowProgress::Terminal {
                status: TerminalStatus::Complete,
                failed_step: None,
                reason: None,
            })
        );
    }

    #[tokio::test]
    async fn progress_terminal_carries_failed_step_and_reason_on_failure() {
        // A step-0 failure (forward_from referencing never-awaited output) fails
        // the run; the terminal progress event names the failing step and a reason.
        let rig = rig(vec![
            ("a", vec![MockScenario::Streaming]),
            ("b", vec![MockScenario::Streaming]),
        ]);
        let sink = Arc::new(RecordingProgressSink::default());
        let yaml = "name: f\ndescription: d\ninputs:\n  a: agent\n  b: agent\nsteps:\n  - label: s\n    send:\n      to: \"{{ b }}\"\n      forward_from: \"{{ a }}\"\n      text: use it\n";
        let (run, _dir, _path) = build_run_with_sink(
            &rig,
            PromptService::disabled(),
            yaml,
            "f",
            supplied(vec![("a", text("a")), ("b", text("b"))]),
            CancellationToken::new(),
            sink.clone(),
        );
        assert_eq!(run.execute().await, RunStatus::Failed);
        let last = sink.snapshot().pop().unwrap();
        match last {
            WorkflowProgress::Terminal {
                status: TerminalStatus::Failed,
                failed_step: Some(0),
                reason: Some(reason),
            } => assert!(!reason.is_empty()),
            other => panic!("expected failed terminal with step+reason, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn back_to_back_same_agent_completes() {
        // Single-agent send→wait→send→wait: the second send re-dispatches to a
        // just-awaited agent, exercising the dispatcher's drain-window accept
        // end-to-end through the interpreter.
        let rig = rig(vec![(
            "w",
            vec![MockScenario::Streaming, MockScenario::Streaming],
        )]);
        let yaml = "name: b\ndescription: d\ninputs:\n  w: agent\nsteps:\n  - label: s\n    send:\n      to: \"{{ w }}\"\n      text: one\n  - label: s\n    wait_for:\n      agent: \"{{ w }}\"\n  - label: s\n    send:\n      to: \"{{ w }}\"\n      text: two\n  - label: s\n    wait_for:\n      agent: \"{{ w }}\"\n";
        let (run, _dir, _path) = build_run(
            &rig,
            PromptService::disabled(),
            yaml,
            "b",
            supplied(vec![("w", text("w"))]),
            CancellationToken::new(),
        );
        assert_eq!(run.execute().await, RunStatus::Complete);
    }

    #[tokio::test]
    async fn cancel_spares_an_unrelated_participant_turn() {
        // Roster [a, b]. The workflow drives only `a`; `b` is busy with a turn the
        // workflow did NOT dispatch. Cancelling the workflow must cancel a's turn
        // but leave b's unrelated turn untouched (the over-cancel fix: cancel
        // targets the run's own in-flight turns, not the declared roster).
        let rig = rig(vec![
            ("a", vec![MockScenario::AwaitCancellation]),
            ("b", vec![MockScenario::AwaitCancellation]),
        ]);
        let a = rig.ids["a"];
        let b = rig.ids["b"];
        // A turn on `b` that the workflow does not own.
        let _manual = rig
            .dispatcher
            .send_message_awaiting_completion(
                b,
                "manual",
                Vec::new(),
                Uuid::now_v7(),
                rig.provider.factory_for(b).unwrap(),
            )
            .await;

        let cancel = CancellationToken::new();
        let yaml = "name: x\ndescription: d\ninputs:\n  a: agent\n  b: agent\nsteps:\n  - label: s\n    send:\n      to: \"{{ a }}\"\n      text: go\n  - label: s\n    wait_for:\n      agent: \"{{ a }}\"\n";
        let (run, _dir, _path) = build_run(
            &rig,
            PromptService::disabled(),
            yaml,
            "x",
            supplied(vec![("a", text("a")), ("b", text("b"))]),
            cancel.clone(),
        );
        let emitter = Arc::clone(&rig.emitter);
        let (status, ()) = tokio::join!(run.execute(), async move {
            // Both a's workflow turn and b's manual turn are in flight.
            emitter.wait_for_type("turn_start", 2).await;
            cancel.cancel();
        });
        assert_eq!(status, RunStatus::Cancelled);
        // b's turn (AwaitCancellation) only ends if cancelled. The workflow must
        // not have cancelled it → no terminal on b's channel.
        let b_channel = format!("agent:{b}");
        let b_terminated = rig.emitter.snapshot().into_iter().any(|(ch, p)| {
            ch == b_channel && p.get("type").and_then(|t| t.as_str()) == Some("turn_end")
        });
        assert!(
            !b_terminated,
            "the unrelated participant turn must survive workflow cancel"
        );
        // Clean up: end b's turn (and a's, already cancelled).
        rig.dispatcher.cancel(b, CancelSource::User);
        let _ = a;
    }

    #[tokio::test]
    async fn same_agent_fire_and_forget_first_failure_fails_run() {
        // Two un-awaited sends to the same agent (the second accepted once the
        // first is terminal). The first turn fails; its outcome must not be lost
        // when the second is tracked — the run is Failed, not Complete.
        let rig = rig(vec![
            (
                "w",
                vec![MockScenario::DispatchFails, MockScenario::Streaming],
            ),
            ("o", vec![MockScenario::Streaming]),
        ]);
        // send w (fails), then send/wait o (gives w's first turn time to settle),
        // then send w again (accepted) — no wait on either w send.
        let yaml = "name: f\ndescription: d\ninputs:\n  w: agent\n  o: agent\nsteps:\n  - label: s\n    send:\n      to: \"{{ w }}\"\n      text: x\n  - label: s\n    send:\n      to: \"{{ o }}\"\n      text: y\n  - label: s\n    wait_for:\n      agent: \"{{ o }}\"\n  - label: s\n    send:\n      to: \"{{ w }}\"\n      text: z\n";
        let (run, _dir, path) = build_run(
            &rig,
            PromptService::disabled(),
            yaml,
            "f",
            supplied(vec![("w", text("w")), ("o", text("o"))]),
            CancellationToken::new(),
        );
        assert_eq!(run.execute().await, RunStatus::Failed);
        assert!(matches!(
            records(&path).last(),
            Some(RunRecord::Terminal {
                status: TerminalStatus::Failed,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn partial_fanout_contention_does_not_cancel_accepted_sibling() {
        // Locks down the spec's partial-dispatch rule (and the rejected review
        // finding): when a fan-out has one recipient accepted and a later one
        // busy, the step fails — but the accepted turn is NOT cancelled; it runs
        // to its natural terminal (its output stays visible).
        let signal = Arc::new(tokio::sync::Notify::new());
        let rig = rig(vec![
            ("a", vec![MockScenario::Streaming]),
            (
                "busy",
                vec![MockScenario::CompletesOnSignal(Arc::clone(&signal))],
            ),
        ]);
        let busy = rig.ids["busy"];
        // Pre-occupy `busy` with a turn the workflow doesn't own.
        let _occupy = rig
            .dispatcher
            .send_message_awaiting_completion(
                busy,
                "occupy",
                Vec::new(),
                Uuid::now_v7(),
                rig.provider.factory_for(busy).unwrap(),
            )
            .await;
        rig.emitter.wait_for_type("turn_start", 1).await;

        // Fan-out to [a, busy] in order: `a` is accepted, `busy` fails fast.
        let yaml = "name: p\ndescription: d\ninputs:\n  rs: [agent]\nsteps:\n  - label: s\n    send:\n      to: \"{{ rs }}\"\n      text: hi\n";
        let (run, _dir, _path) = build_run(
            &rig,
            PromptService::disabled(),
            yaml,
            "p",
            supplied(vec![("rs", list(&["a", "busy"]))]),
            CancellationToken::new(),
        );
        assert_eq!(run.execute().await, RunStatus::Failed);
        // `a`'s accepted turn runs to a completed terminal — the failure did not
        // cancel it. (Would time out here if it had been cancelled instead.)
        let a_channel = format!("agent:{}", rig.ids["a"]);
        rig.emitter
            .wait_for(|events| {
                events.iter().any(|(ch, p)| {
                    *ch == a_channel
                        && p.get("type").and_then(|t| t.as_str()) == Some("turn_end")
                        && p.get("outcome")
                            .and_then(|o| o.get("status"))
                            .and_then(|s| s.as_str())
                            == Some("completed")
                })
            })
            .await;
        signal.notify_one();
    }
}
