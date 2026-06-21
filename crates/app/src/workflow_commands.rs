//! App-side workflow command surface: discovery (`list`), invocation
//! (`validate` + `invoke`), live control (`cancel`), and crash/failure recovery
//! (`list_workflow_runs` + `abandon`), plus the production glue the interpreter
//! needs — a [`DispatchFactoryProvider`] backed by `AppState` and a
//! [`ProgressSink`] that mirrors run progress onto the project-scoped
//! `workflow:<project-id>` channel and into the in-memory active-run registry.
//!
//! The interpreter ([`crate::workflow`]) is the pure conductor; this module is
//! where it meets `AppState`: building per-agent dispatch factories, spawning the
//! run on a background task, and applying the retention policy on terminal.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use switchboard_core::name::canonicalize_for_uniqueness;
use switchboard_core::{AgentId, AgentRecord, Project, ProjectId};
use switchboard_dispatcher::{DispatchContextFactory, EventEmitter};
use switchboard_workflow::{
    InputType, InputValue, RunRecord, RunStatus, TerminalStatus, Workflow, bind_invocation,
    builtin_workflow, builtin_workflow_content, builtin_workflows, parse_workflow,
    validate_invocation,
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::commands::adapter_for;
use crate::dispatch_context::ProjectDispatchContextFactory;
use crate::error::AppError;
use crate::state::{ActiveRun, AppState, RunSnapshot, lock};
use crate::workflow::{DispatchFactoryProvider, ProgressSink, WorkflowProgress, WorkflowRun};

/// The project-scoped progress channel name. The frontend subscribes once per
/// loaded project (not per run); the payload's `run_id` disambiguates a project's
/// concurrent runs.
#[must_use]
pub fn progress_channel(project_id: ProjectId) -> String {
    format!("workflow:{project_id}")
}

/// Fires OS notifications for a workflow run's terminal. Injected into `AppState`
/// so the run's background task can notify without depending on Tauri internals;
/// the production impl wraps the notification plugin and suppresses when the
/// window is focused, while tests record calls. Called **only** for completion
/// and failure — never cancel or interruption.
pub trait Notifier: Send + Sync {
    fn notify(&self, title: &str, body: &str);
}

/// A notifier that drops every call — the default until production injects a real
/// one, and the choice for headless tests that don't assert on notifications.
pub struct NullNotifier;

impl Notifier for NullNotifier {
    fn notify(&self, _title: &str, _body: &str) {}
}

/// Builds a per-send [`DispatchContextFactory`] for each agent a run may target.
/// Constructed at invoke from the run's project roster. An agent on an unsupported
/// harness is **omitted** (not a hard failure), so `factory_for` returns `None`
/// for it — the interpreter turns that into a clean step failure on the step that
/// targets it, and a workflow that doesn't target it runs fine. (Failing the
/// whole invoke on one such agent would block *every* workflow in the project,
/// even ones unrelated to it.) Each factory still reads `agents_by_id` live at
/// `build()` time, so a runtime-captured session locator is honored on the next
/// turn — this provider does not freeze that.
pub struct ProjectDispatchFactoryProvider {
    factories: HashMap<AgentId, Arc<dyn DispatchContextFactory>>,
}

impl ProjectDispatchFactoryProvider {
    /// Build a factory for every supported-harness agent in `roster` (the run's
    /// project roster, a superset of the agents the run actually targets).
    #[must_use]
    pub fn new(state: &AppState, project: &Project, roster: &[AgentRecord]) -> Self {
        let mut factories: HashMap<AgentId, Arc<dyn DispatchContextFactory>> = HashMap::new();
        for agent in roster {
            let Ok(adapter) = adapter_for(state, agent) else {
                tracing::warn!(
                    agent = %agent.id,
                    harness = ?agent.harness,
                    "omitting agent from workflow dispatch — no adapter for its harness"
                );
                continue;
            };
            let factory = ProjectDispatchContextFactory::new(
                project.clone(),
                agent.clone(),
                adapter,
                Arc::clone(&state.emitter),
                Arc::clone(&state.needs_session_meta),
                Arc::clone(&state.agents_by_id),
                Arc::clone(&state.registry_write),
            );
            factories.insert(
                agent.id,
                Arc::new(factory) as Arc<dyn DispatchContextFactory>,
            );
        }
        Self { factories }
    }
}

impl DispatchFactoryProvider for ProjectDispatchFactoryProvider {
    fn factory_for(&self, agent_id: AgentId) -> Option<Arc<dyn DispatchContextFactory>> {
        self.factories.get(&agent_id).cloned()
    }
}

/// The `workflow:<project-id>` event payload. Carries orchestration state only —
/// **no agent output text** (§3); the turns stream on `agent:<id>` and render in
/// the transcript. `step` is the zero-based index of the step in progress.
#[derive(Debug, Clone, Serialize)]
pub struct WorkflowProgressPayload {
    pub run_id: String,
    pub workflow: String,
    pub step: usize,
    pub total: usize,
    /// `"running"` while live; `"complete"` / `"cancelled"` / `"failed"` at terminal.
    pub status: &'static str,
    /// The failure reason on a failed terminal, else `None`.
    pub reason: Option<String>,
}

/// The production [`ProgressSink`]: mirrors the interpreter's live progress onto
/// the project-scoped channel and updates the active-run registry's step
/// snapshot, so `list_workflow_runs` reports a live run's step without re-reading
/// disk. Lives on the run's `'static` background task.
pub struct ChannelProgressSink {
    run_id: uuid::Uuid,
    project_id: ProjectId,
    workflow: String,
    total_steps: usize,
    emitter: Arc<dyn EventEmitter>,
    workflow_runs: Arc<Mutex<HashMap<uuid::Uuid, ActiveRun>>>,
}

impl ChannelProgressSink {
    pub fn new(
        run_id: uuid::Uuid,
        project_id: ProjectId,
        workflow: String,
        total_steps: usize,
        emitter: Arc<dyn EventEmitter>,
        workflow_runs: Arc<Mutex<HashMap<uuid::Uuid, ActiveRun>>>,
    ) -> Self {
        Self {
            run_id,
            project_id,
            workflow,
            total_steps,
            emitter,
            workflow_runs,
        }
    }
}

impl ProgressSink for ChannelProgressSink {
    fn emit(&self, event: WorkflowProgress) {
        // Update the live registry snapshot (step granularity) for step-start
        // events; the entry is still present at terminal (the owning task removes
        // it only after `execute()` returns). Tail-leaf lock, no await held.
        let (step, status, reason) = match event {
            WorkflowProgress::Started { .. } => (0, "running", None),
            WorkflowProgress::StepStarted { step_index } => {
                if let Some(run) = lock(&self.workflow_runs).get_mut(&self.run_id) {
                    run.snapshot.current_step = step_index;
                }
                (step_index, "running", None)
            }
            // Report the actual terminal step so the live indicator matches what
            // `list_workflow_runs` reports after a refresh: the failing step on a
            // failure, else the latest step the run reached (from the snapshot,
            // still present here — the owning task removes the entry only after
            // `execute()` returns).
            WorkflowProgress::Terminal {
                status,
                failed_step,
                reason,
            } => {
                let step = failed_step.unwrap_or_else(|| {
                    lock(&self.workflow_runs)
                        .get(&self.run_id)
                        .map_or(0, |run| run.snapshot.current_step)
                });
                (step, terminal_status_str(status), reason)
            }
        };
        let payload = WorkflowProgressPayload {
            run_id: self.run_id.to_string(),
            workflow: self.workflow.clone(),
            step,
            total: self.total_steps,
            status,
            reason,
        };
        self.emitter.emit(
            &progress_channel(self.project_id),
            serde_json::to_value(payload).unwrap_or(serde_json::Value::Null),
        );
    }
}

fn terminal_status_str(status: TerminalStatus) -> &'static str {
    match status {
        TerminalStatus::Complete => "complete",
        TerminalStatus::Cancelled => "cancelled",
        // `Failed` and the `#[non_exhaustive]` catch-all both degrade to "failed"
        // rather than failing the build on a future variant.
        _ => "failed",
    }
}

// --- Wire types --------------------------------------------------------------

/// One declared input as the invocation form needs it: name, base type, whether
/// it's optional, and its description (in declaration order).
#[derive(Debug, Clone, Serialize)]
pub struct WorkflowInputInfo {
    pub name: String,
    /// `"agent"` | `"agent_list"` | `"prompt_id"` | `"text"` | `"text_list"`.
    pub ty: String,
    pub optional: bool,
    pub description: Option<String>,
}

/// A workflow as the menu/list shows it: parsed metadata **or** a parse error,
/// plus the read-only/built-in flag and the up-front `invocable` flag.
#[derive(Debug, Clone, Serialize)]
pub struct WorkflowListing {
    pub name: String,
    pub is_builtin: bool,
    /// `None` when `parse_error` is set (a malformed directory file).
    pub description: Option<String>,
    pub inputs: Vec<WorkflowInputInfo>,
    /// False when the workflow uses a gated step (`pause_for_user`/`for_each`),
    /// so the UI can disable invoke up front.
    pub invocable: bool,
    /// The parse error for a malformed directory file; `None` otherwise.
    pub parse_error: Option<String>,
    /// Recommended prompt id per `prompt_id` input (built-ins only), for form
    /// pre-selection. Keyed by input name; empty for user workflows.
    pub recommended_prompts: BTreeMap<String, String>,
}

/// A run as the indicator shows it. `status` is `"running"` (live),
/// `"failed"` (retained terminal), or `"interrupted"` (no terminal, not live).
#[derive(Debug, Clone, Serialize)]
pub struct WorkflowRunInfo {
    pub run_id: String,
    pub workflow: String,
    pub step: usize,
    pub total: usize,
    pub status: &'static str,
    pub reason: Option<String>,
}

fn input_type_str(ty: InputType) -> &'static str {
    match ty {
        InputType::Agent => "agent",
        InputType::AgentList => "agent_list",
        InputType::PromptId => "prompt_id",
        InputType::Text => "text",
        InputType::TextList => "text_list",
    }
}

fn input_infos(workflow: &Workflow) -> Vec<WorkflowInputInfo> {
    workflow
        .inputs
        .iter()
        .map(|i| WorkflowInputInfo {
            name: i.name.clone(),
            ty: input_type_str(i.ty).to_owned(),
            optional: i.optional,
            description: i.description.clone(),
        })
        .collect()
}

/// Recommended built-in prompt id per `prompt_id` input, keyed by built-in
/// workflow name. App-owned (built-ins are app-owned), so this needs no DSL
/// change; a user workflow's `prompt_id` input gets no pre-selection. Pre-selecting
/// the co-versioned built-ins keeps the default path render-consistent.
fn recommended_prompts_for(workflow_name: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    match workflow_name {
        "review-analyze-discuss" => {
            map.insert("review_prompt".to_owned(), "builtin:code-review".to_owned());
            map.insert(
                "analysis_prompt".to_owned(),
                "builtin:ai-review-feedback".to_owned(),
            );
        }
        "review-and-aggregate" => {
            map.insert("review_prompt".to_owned(), "builtin:code-review".to_owned());
        }
        _ => {}
    }
    map
}

fn builtin_listing(workflow: &Workflow) -> WorkflowListing {
    WorkflowListing {
        name: workflow.name.clone(),
        is_builtin: true,
        description: Some(workflow.description.clone()),
        inputs: input_infos(workflow),
        invocable: workflow.gated_step_kind().is_none(),
        parse_error: None,
        recommended_prompts: recommended_prompts_for(&workflow.name),
    }
}

// --- list --------------------------------------------------------------------

/// All workflows: the read-only built-in library (governed by the `show_builtins`
/// preference) merged with the user-global workflows folder's own `*.{yaml,yml}`
/// files. User-global — the same set is available from every project. A malformed
/// file surfaces its parse error in place rather than failing the whole list;
/// built-ins, being baked-in, never produce a parse error.
pub fn list_workflows_impl(state: &AppState) -> Vec<WorkflowListing> {
    let mut out = Vec::new();
    if lock(&state.preferences).show_builtins {
        out.extend(builtin_workflows().iter().map(builtin_listing));
    }
    if let Some(dir) = state.workflows_dir.as_deref() {
        out.extend(user_workflow_listings(dir));
    }
    out
}

/// Scan the user-global workflows folder's `*.{yaml,yml}` files into listings
/// (parsed metadata or the parse error), one entry per file.
fn user_workflow_listings(workflows_dir: &Path) -> Vec<WorkflowListing> {
    let mut listings = Vec::new();
    for (stem, content) in read_workflow_files(workflows_dir) {
        match parse_workflow(&stem, &content) {
            Ok(workflow) => listings.push(WorkflowListing {
                name: workflow.name.clone(),
                is_builtin: false,
                description: Some(workflow.description.clone()),
                inputs: input_infos(&workflow),
                invocable: workflow.gated_step_kind().is_none(),
                parse_error: None,
                recommended_prompts: BTreeMap::new(),
            }),
            Err(e) => listings.push(WorkflowListing {
                name: stem,
                is_builtin: false,
                description: None,
                inputs: Vec::new(),
                invocable: false,
                parse_error: Some(e.to_string()),
                recommended_prompts: BTreeMap::new(),
            }),
        }
    }
    listings
}

/// `(file_stem, content)` for every `*.{yaml,yml}` file in `dir`, sorted by stem.
/// A missing dir yields nothing; an unreadable file is skipped with a warning.
fn read_workflow_files(dir: &Path) -> Vec<(String, String)> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            tracing::warn!(dir = %dir.display(), error = %e, "cannot read workflows directory");
            return Vec::new();
        }
    };
    let mut files: Vec<(String, String)> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("yaml") || ext.eq_ignore_ascii_case("yml"))
        })
        .filter_map(|path| {
            let stem = path.file_stem()?.to_string_lossy().into_owned();
            match std::fs::read_to_string(&path) {
                Ok(content) => Some((stem, content)),
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "skipping unreadable workflow file");
                    None
                }
            }
        })
        .collect();
    files.sort_by(|a, b| a.0.cmp(&b.0));
    files
}

// --- resolution + validation -------------------------------------------------

/// The agent roster (records) for a project, from the register cache.
fn roster_for_project(state: &AppState, project_id: ProjectId) -> Vec<AgentRecord> {
    lock(&state.agents_by_id)
        .values()
        .filter(|r| r.project_id == project_id)
        .cloned()
        .collect()
}

/// The user-global workflows directory, or a clear error if no config dir was
/// resolved (an exotic host with no home).
fn workflows_dir(state: &AppState) -> Result<&Path, AppError> {
    state
        .workflows_dir
        .as_deref()
        .ok_or(AppError::WorkflowsDirUnavailable)
}

/// Re-read and parse the named workflow snapshot: a built-in from the library, or
/// a user-global directory file (re-read once at invoke, never per step).
fn snapshot_workflow(state: &AppState, name: &str, is_builtin: bool) -> Result<Workflow, AppError> {
    if is_builtin {
        return builtin_workflow(name).ok_or_else(|| AppError::WorkflowNotFound {
            name: name.to_owned(),
        });
    }
    let dir = workflows_dir(state)?;
    for ext in ["yaml", "yml"] {
        let path = dir.join(format!("{name}.{ext}"));
        if path.exists() {
            let content =
                std::fs::read_to_string(&path).map_err(|source| AppError::WorkflowCopyIo {
                    path: path.clone(),
                    source,
                })?;
            return Ok(parse_workflow(name, &content)?);
        }
    }
    Err(AppError::WorkflowNotFound {
        name: name.to_owned(),
    })
}

/// True if `id` (a `provider:name` prompt address) resolves against the prompt
/// cache. Built-ins are always present; local/MCP reflect the last sync.
fn prompt_resolves(state: &AppState, id: &str) -> bool {
    match switchboard_prompts::PromptId::parse(id) {
        Ok(parsed) => state
            .prompts
            .list()
            .iter()
            .any(|p| p.provider == parsed.provider && p.name == parsed.name),
        Err(_) => false,
    }
}

/// Validate a workflow invocation: capability gate + M3's invocation rules
/// (against the project roster and a `PromptService`-backed prompt predicate).
pub fn validate_workflow_invocation_impl(
    state: &AppState,
    project_id: ProjectId,
    name: &str,
    is_builtin: bool,
    inputs: &BTreeMap<String, InputValue>,
) -> Result<(), AppError> {
    let workflow = snapshot_workflow(state, name, is_builtin)?;
    if workflow.gated_step_kind().is_some() {
        return Err(AppError::WorkflowStepUnsupported);
    }
    let roster = roster_for_project(state, project_id);
    let names: Vec<String> = roster.iter().map(|r| r.name.clone()).collect();
    validate_invocation(&workflow, inputs, &names, |id| prompt_resolves(state, id))?;
    Ok(())
}

// --- invoke ------------------------------------------------------------------

/// Validate, bind, snapshot, and launch a workflow run on a background task,
/// returning its `run_id` immediately. The interpreter dispatches every step
/// backend-side; there is no per-step frontend round-trip.
pub fn invoke_workflow_impl(
    state: &AppState,
    project_id: ProjectId,
    name: &str,
    is_builtin: bool,
    inputs: &BTreeMap<String, InputValue>,
) -> Result<Uuid, AppError> {
    let workflow = snapshot_workflow(state, name, is_builtin)?;
    if workflow.gated_step_kind().is_some() {
        return Err(AppError::WorkflowStepUnsupported);
    }

    let project = lock(&state.projects)
        .get(&project_id)
        .cloned()
        .ok_or(AppError::ProjectNotLoaded(project_id))?;
    let roster = roster_for_project(state, project_id);
    let names: Vec<String> = roster.iter().map(|r| r.name.clone()).collect();

    // Validate + bind against the roster and prompt resolver.
    let bound = bind_invocation(&workflow, inputs, &names, |id| prompt_resolves(state, id))?;
    let agents: BTreeMap<String, AgentId> = roster
        .iter()
        .map(|r| (canonicalize_for_uniqueness(&r.name), r.id))
        .collect();

    let provider = Arc::new(ProjectDispatchFactoryProvider::new(
        state, &project, &roster,
    ));

    let run_id = Uuid::now_v7();
    let run_path = project.run_path(run_id);
    let total_steps = workflow.steps.len();
    let workflow_name = workflow.name.clone();
    let cancel = CancellationToken::new();
    let done = Arc::new(tokio::sync::Notify::new());

    let sink = Arc::new(ChannelProgressSink::new(
        run_id,
        project_id,
        workflow_name.clone(),
        total_steps,
        Arc::clone(&state.emitter),
        Arc::clone(&state.workflow_runs),
    ));

    // Register the live run before spawning so cancel/list see it immediately.
    lock(&state.workflow_runs).insert(
        run_id,
        ActiveRun {
            cancel: cancel.clone(),
            project_id,
            workflow: workflow_name.clone(),
            snapshot: RunSnapshot {
                total_steps,
                current_step: 0,
            },
            done: Arc::clone(&done),
        },
    );

    let run = WorkflowRun {
        workflow,
        inputs: bound,
        agents,
        dispatcher: Arc::clone(&state.dispatcher),
        prompts: state.prompts.clone(),
        factories: provider,
        run_path: run_path.clone(),
        progress: sink,
        cancel,
    };

    let workflow_runs = Arc::clone(&state.workflow_runs);
    let notifier = Arc::clone(&state.notifier);
    tokio::spawn(async move {
        let status = run.execute().await;
        // The run is terminal: drop the registry entry, apply retention, then
        // signal completion (`notify_one` so a teardown waiter that hasn't parked
        // yet still gets the wakeup), and finally notify the user.
        lock(&workflow_runs).remove(&run_id);
        apply_retention(&run_path, status);
        done.notify_one();
        match status {
            RunStatus::Complete => {
                notifier.notify("Workflow complete", &format!("{workflow_name} finished."));
            }
            RunStatus::Failed => {
                notifier.notify("Workflow failed", &format!("{workflow_name} failed."));
            }
            // No notification on user-initiated cancel; interruption has no live
            // process and is surfaced in the indicator on restart instead.
            _ => {}
        }
    });

    Ok(run_id)
}

/// Retention keyed off the returned status: prune a `Complete`/`Cancelled` run
/// file; retain a `Failed` file (surfaced until abandon). `Interrupted` is never
/// produced here (it exists only because the process died).
fn apply_retention(run_path: &Path, status: RunStatus) {
    if !matches!(status, RunStatus::Complete | RunStatus::Cancelled) {
        return;
    }
    match std::fs::remove_file(run_path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            tracing::warn!(path = %run_path.display(), error = %e, "could not prune workflow run file");
        }
    }
}

// --- cancel ------------------------------------------------------------------

/// Fire the run's cancel token; the interpreter observes it and finishes
/// `cancelled`. No-op if the run already finished (a stale cancel is harmless).
pub fn cancel_workflow_run_impl(state: &AppState, run_id: Uuid) {
    let token = lock(&state.workflow_runs)
        .get(&run_id)
        .map(|run| run.cancel.clone());
    if let Some(token) = token {
        token.cancel();
    }
}

// --- list runs ---------------------------------------------------------------

/// All runs the indicator should show for a project: live runs (from the
/// registry), retained **failed** runs, and **interrupted** runs (a run file with
/// no terminal record, not in the live registry). Complete/cancelled files were
/// pruned on terminal and do not appear.
pub fn list_workflow_runs_impl(state: &AppState, project_id: ProjectId) -> Vec<WorkflowRunInfo> {
    let mut out = Vec::new();
    let mut live_ids: Vec<Uuid> = Vec::new();
    {
        let runs = lock(&state.workflow_runs);
        for (id, run) in runs.iter() {
            if run.project_id != project_id {
                continue;
            }
            live_ids.push(*id);
            out.push(WorkflowRunInfo {
                run_id: id.to_string(),
                workflow: run.workflow.clone(),
                step: run.snapshot.current_step,
                total: run.snapshot.total_steps,
                status: "running",
                reason: None,
            });
        }
    }

    // Scan the project's run files for retained-failed and interrupted runs not
    // already represented by a live registry entry.
    let project = lock(&state.projects).get(&project_id).cloned();
    if let Some(project) = project {
        for (run_id, records) in read_run_files(&project.runs_dir()) {
            if live_ids.contains(&run_id) {
                continue;
            }
            if let Some(info) = classify_run_file(run_id, &records) {
                out.push(info);
            }
        }
    }
    out
}

/// `(run_id, records)` for every parseable `*.jsonl` in `runs_dir`.
fn read_run_files(runs_dir: &Path) -> Vec<(Uuid, Vec<RunRecord>)> {
    let Ok(entries) = std::fs::read_dir(runs_dir) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "jsonl"))
        .filter_map(|path| {
            let run_id: Uuid = path.file_stem()?.to_string_lossy().parse().ok()?;
            let records = switchboard_core::read_jsonl::<RunRecord>(&path).ok()?;
            Some((run_id, records))
        })
        .collect()
}

/// Classify a run file (not in the live registry) into the run info the indicator
/// shows, or `None` if it is a pruned/complete/cancelled terminal that shouldn't
/// surface.
fn classify_run_file(run_id: Uuid, records: &[RunRecord]) -> Option<WorkflowRunInfo> {
    let mut workflow = String::new();
    let mut total = 0usize;
    let mut completed = 0usize;
    let mut terminal: Option<(TerminalStatus, Option<usize>, Option<String>)> = None;
    for record in records {
        match record {
            RunRecord::Started {
                workflow: name,
                total_steps,
                ..
            } => {
                workflow.clone_from(name);
                total = *total_steps;
            }
            RunRecord::StepCompleted { .. } => completed += 1,
            RunRecord::Terminal {
                status,
                failed_step,
                reason,
                ..
            } => terminal = Some((*status, *failed_step, reason.clone())),
            _ => {}
        }
    }
    match terminal {
        // A failed terminal is retained and surfaced for abandon.
        Some((TerminalStatus::Failed, failed_step, reason)) => Some(WorkflowRunInfo {
            run_id: run_id.to_string(),
            workflow,
            step: failed_step.unwrap_or(completed),
            total,
            status: "failed",
            reason,
        }),
        // Complete/cancelled should already be pruned; never surface it.
        Some(_) => None,
        // No terminal record → the process died mid-run.
        None => Some(WorkflowRunInfo {
            run_id: run_id.to_string(),
            workflow,
            step: completed,
            total,
            status: "interrupted",
            reason: None,
        }),
    }
}

// --- abandon -----------------------------------------------------------------

/// Delete a `failed` or `interrupted` run file (the run indicator's Abandon
/// action). Complete/cancelled files were already pruned on terminal. Refuses a
/// **live** run — deleting its file would just be recreated on the interpreter's
/// next record; the run must be cancelled first.
pub fn abandon_workflow_run_impl(
    state: &AppState,
    project_id: ProjectId,
    run_id: Uuid,
) -> Result<(), AppError> {
    if lock(&state.workflow_runs).contains_key(&run_id) {
        return Err(AppError::WorkflowRunActive { run_id });
    }
    let project = lock(&state.projects)
        .get(&project_id)
        .cloned()
        .ok_or(AppError::ProjectNotLoaded(project_id))?;
    let path = project.run_path(run_id);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Err(AppError::WorkflowRunNotFound { run_id })
        }
        Err(source) => Err(AppError::WorkflowCopyIo { path, source }),
    }
}

// --- copy built-in to my workflows -------------------------------------------

/// Copy a built-in workflow into the directory's `workflows/` folder as
/// `<name>.yaml`, refusing to overwrite an existing file. Thereafter it lists as
/// a normal (non-built-in) directory workflow.
pub fn copy_builtin_workflow_impl(name: &str, workflows_dir: &Path) -> Result<PathBuf, AppError> {
    let content = builtin_workflow_content(name).ok_or_else(|| AppError::WorkflowNotFound {
        name: name.to_owned(),
    })?;
    let dest = workflows_dir.join(format!("{name}.yaml"));
    // Refuse if *either* extension already holds this name — the directory scan
    // and `snapshot_workflow` both treat `.yaml`/`.yml` as the same workflow, so
    // creating `<name>.yaml` next to an existing `<name>.yml` would be a duplicate.
    for ext in ["yaml", "yml"] {
        let existing = workflows_dir.join(format!("{name}.{ext}"));
        if existing.exists() {
            return Err(AppError::WorkflowCopyExists { path: existing });
        }
    }
    std::fs::create_dir_all(workflows_dir).map_err(|source| AppError::WorkflowCopyIo {
        path: workflows_dir.to_path_buf(),
        source,
    })?;
    std::fs::write(&dest, content).map_err(|source| AppError::WorkflowCopyIo {
        path: dest.clone(),
        source,
    })?;
    Ok(dest)
}

/// The user-global workflows folder — where "Copy to my workflows" writes and
/// "Open workflows folder" opens. Errors if no config dir was resolved.
pub fn user_workflows_dir(state: &AppState) -> Result<PathBuf, AppError> {
    workflows_dir(state).map(Path::to_path_buf)
}

// --- teardown ----------------------------------------------------------------

/// Hard ceiling on how long teardown waits for cancelled runs to settle before
/// it proceeds anyway. A wedged run mustn't block an irreversible
/// directory-removal/project-deletion forever; if the deadline elapses we proceed
/// **with a warning** so the stuck run is diagnosable (the alternative — a silent
/// fall-through — is what hides a later misclassified run).
const RUN_TEARDOWN_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);

/// Cancel all live runs for the given projects and await them reaching terminal,
/// **before** the caller drains agents. Collects each run's cancel token **and
/// its completion signal** under the registry lock (so the owning task removing
/// its own entry can't strand the waiter), releases the lock, fires the cancels,
/// then awaits the signals under one bounded deadline. Awaiting terminal here is
/// what makes each run resolve `cancelled` (not `failed`) and stops it dispatching
/// against state about to be unloaded; on deadline we warn and proceed.
pub async fn cancel_runs_for_projects(state: &AppState, project_ids: &[ProjectId]) {
    let handles: Vec<(Uuid, CancellationToken, Arc<tokio::sync::Notify>)> = {
        let runs = lock(&state.workflow_runs);
        runs.iter()
            .filter(|(_, run)| project_ids.contains(&run.project_id))
            .map(|(id, run)| (*id, run.cancel.clone(), Arc::clone(&run.done)))
            .collect()
    };
    if handles.is_empty() {
        return;
    }
    for (_, token, _) in &handles {
        token.cancel();
    }
    // Await every run's completion signal concurrently under one deadline. The
    // tasks fire `notify_one` after removing their registry entries, so by the
    // time a wait resolves the entry is gone and the run file pruned/retained.
    let waits = handles
        .iter()
        .map(|(_, _, done)| done.notified())
        .collect::<Vec<_>>();
    if tokio::time::timeout(RUN_TEARDOWN_DEADLINE, futures::future::join_all(waits))
        .await
        .is_err()
    {
        let stuck: Vec<Uuid> = {
            let runs = lock(&state.workflow_runs);
            handles
                .iter()
                .filter(|(id, _, _)| runs.contains_key(id))
                .map(|(id, _, _)| *id)
                .collect()
        };
        tracing::warn!(
            ?stuck,
            "teardown proceeding before all cancelled workflow runs settled; \
             a stuck run may resolve via the wrong path"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use switchboard_workflow::TerminalStatus;

    /// Captures the most recent emitted (channel, payload) for assertions.
    struct CapturingEmitter {
        last: Mutex<Option<(String, serde_json::Value)>>,
    }
    impl EventEmitter for CapturingEmitter {
        fn emit(&self, name: &str, payload: serde_json::Value) {
            *self.last.lock().unwrap() = Some((name.to_owned(), payload));
        }
    }

    fn register(
        runs: &Arc<Mutex<HashMap<Uuid, ActiveRun>>>,
        run_id: Uuid,
        project_id: ProjectId,
        current_step: usize,
        total_steps: usize,
    ) {
        runs.lock().unwrap().insert(
            run_id,
            ActiveRun {
                cancel: CancellationToken::new(),
                project_id,
                workflow: "w".to_owned(),
                snapshot: RunSnapshot {
                    total_steps,
                    current_step,
                },
                done: Arc::new(tokio::sync::Notify::new()),
            },
        );
    }

    #[test]
    fn terminal_progress_reports_the_failing_step_not_total_minus_one() {
        let project_id = Uuid::now_v7();
        let run_id = Uuid::now_v7();
        let runs = Arc::new(Mutex::new(HashMap::new()));
        register(&runs, run_id, project_id, 3, 8);
        let emitter = Arc::new(CapturingEmitter {
            last: Mutex::new(None),
        });
        let sink = ChannelProgressSink::new(
            run_id,
            project_id,
            "w".to_owned(),
            8,
            Arc::clone(&emitter) as Arc<dyn EventEmitter>,
            Arc::clone(&runs),
        );

        sink.emit(WorkflowProgress::Terminal {
            status: TerminalStatus::Failed,
            failed_step: Some(3),
            reason: Some("boom".to_owned()),
        });

        let (channel, payload) = emitter.last.lock().unwrap().clone().unwrap();
        assert_eq!(channel, format!("workflow:{project_id}"));
        // The failing step (3), not `total - 1` (7) — consistent with what
        // `list_workflow_runs` reports for the same run after a refresh.
        assert_eq!(payload["step"], 3);
        assert_eq!(payload["total"], 8);
        assert_eq!(payload["status"], "failed");
        assert_eq!(payload["reason"], "boom");
    }

    #[test]
    fn terminal_progress_for_non_failure_uses_the_latest_step() {
        // Complete/cancelled carry no `failed_step`; the live terminal step then
        // comes from the snapshot's latest `current_step`, not `total - 1`.
        let project_id = Uuid::now_v7();
        let run_id = Uuid::now_v7();
        let runs = Arc::new(Mutex::new(HashMap::new()));
        register(&runs, run_id, project_id, 2, 8);
        let emitter = Arc::new(CapturingEmitter {
            last: Mutex::new(None),
        });
        let sink = ChannelProgressSink::new(
            run_id,
            project_id,
            "w".to_owned(),
            8,
            Arc::clone(&emitter) as Arc<dyn EventEmitter>,
            Arc::clone(&runs),
        );

        sink.emit(WorkflowProgress::Terminal {
            status: TerminalStatus::Cancelled,
            failed_step: None,
            reason: None,
        });

        let (_, payload) = emitter.last.lock().unwrap().clone().unwrap();
        assert_eq!(payload["step"], 2);
        assert_eq!(payload["status"], "cancelled");
    }
}
