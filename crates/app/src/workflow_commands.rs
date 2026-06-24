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

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use switchboard_core::name::canonicalize_for_uniqueness;
use switchboard_core::{AgentId, AgentRecord, Project, ProjectId};
use switchboard_dispatcher::{DispatchContextFactory, EventEmitter};
use switchboard_prompts::{PromptArgument, PromptId, PromptService};
use switchboard_workflow::{
    InputType, InputValue, RecipientRef, RunRecord, RunStatus, ScopeValue, Step, TerminalStatus,
    Workflow, WorkflowError, WorkflowStepInfo, bind_invocation, builtin_workflow,
    builtin_workflow_content, builtin_workflows, parse_workflow, step_display,
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
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WorkflowInputInfo {
    pub name: String,
    /// `"agent"` | `"agent_list"` | `"text"` | `"text_list"`.
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
    /// Per-step display info for the live progress view. **Resolved** recipients
    /// (concrete agent names) for a live run, sourced from the registry; *declared*
    /// recipients for a disk-sourced failed/interrupted run, reconstructed from the
    /// run file's snapshot (no binding snapshot is journaled). May be empty for a
    /// legacy run file written before the snapshot existed.
    pub steps: Vec<WorkflowStepInfo>,
}

/// Resolve a declared step snapshot's recipients against the invocation's bindings,
/// for the live progress view. A `Slot { input }` expands to one `Literal` per
/// bound agent name (a single `agent` → one, an `[agent]` → the list); an unbound
/// slot is left as-is. `Literal` references pass through unchanged.
fn resolve_step_display(
    declared: &[WorkflowStepInfo],
    bound: &BTreeMap<String, ScopeValue>,
) -> Vec<WorkflowStepInfo> {
    let resolve = |refs: &[RecipientRef]| -> Vec<RecipientRef> {
        refs.iter()
            .flat_map(|r| match r {
                RecipientRef::Slot { input } => match bound.get(input) {
                    Some(ScopeValue::Text(name)) => {
                        vec![RecipientRef::Literal { name: name.clone() }]
                    }
                    Some(ScopeValue::List(names)) => names
                        .iter()
                        .map(|name| RecipientRef::Literal { name: name.clone() })
                        .collect(),
                    None => vec![r.clone()],
                },
                RecipientRef::Literal { .. } => vec![r.clone()],
            })
            .collect()
    };
    declared
        .iter()
        .map(|s| WorkflowStepInfo {
            kind: s.kind,
            label: s.label.clone(),
            description: s.description.clone(),
            prompt: s.prompt.clone(),
            recipients: resolve(&s.recipients),
            feeds_from: resolve(&s.feeds_from),
        })
        .collect()
}

fn input_type_str(ty: InputType) -> &'static str {
    match ty {
        InputType::Agent => "agent",
        InputType::AgentList => "agent_list",
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

fn builtin_listing(workflow: &Workflow) -> WorkflowListing {
    WorkflowListing {
        name: workflow.name.clone(),
        is_builtin: true,
        description: Some(workflow.description.clone()),
        inputs: input_infos(workflow),
        invocable: workflow.gated_step_kind().is_none(),
        parse_error: None,
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
            }),
            Err(e) => listings.push(WorkflowListing {
                name: stem,
                is_builtin: false,
                description: None,
                inputs: Vec::new(),
                invocable: false,
                parse_error: Some(e.to_string()),
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

// --- prompt-schema resolution + binding classification -----------------------
//
// A workflow step names its prompt as a hardcoded literal; the prompt's
// user-fillable arguments are auto-derived from the resolved prompt, not declared
// as inputs. The single primitive below resolves a prompt id to its declared
// arguments and the classification rules turn each `send`'s bindings into form
// fields + a compatibility verdict. Reused by the form descriptor, invoke
// pre-flight, and (via `resolve_prompt_schema`) the runtime arg assembly, so the
// three never derive prompt schemas three different ways.

/// Build an invocation-rejected [`WorkflowError`]. The crate's own `invocation`
/// constructor is `pub(crate)`, so we construct the public variant directly —
/// these app-layer rejections map to `AppError::Workflow` like the crate's own.
fn invocation_msg(message: String) -> WorkflowError {
    WorkflowError::Invocation { message }
}

/// One hardcoded prompt's schema-resolution outcome.
enum PromptSchema {
    /// Resolved to its declared arguments (possibly empty).
    Resolved(Vec<PromptArgument>),
    /// A `builtin`/`local` id that doesn't resolve — **definitively missing**. These
    /// providers resolve directly (compiled-in / filesystem scan), so a miss can
    /// never become present via a sync (e.g. a `local:ghost` typo). A blocking
    /// error, surfaced immediately — *not* `Unresolved`, which would spin forever.
    Missing,
    /// An **MCP** id absent from the cache — possibly just a cold/stale cache
    /// pending a sync, so not a hard error. The form treats this as pending and
    /// re-checks after a sync; only still-missing-after-a-settled-sync is an error.
    Unresolved,
    /// The literal isn't a `provider:name` id — a malformed workflow definition.
    Malformed,
}

/// Resolve a hardcoded prompt id to its declared arguments via the prompt cache.
/// Provider-aware on a miss: `builtin`/`local` resolve directly (so a miss is
/// definitively `Missing`), while an MCP miss is `Unresolved` (a sync may still
/// make it appear).
fn resolve_prompt_schema(prompts: &PromptService, id: &str) -> PromptSchema {
    match PromptId::parse(id) {
        Ok(pid) => match prompts.get(&pid.provider, &pid.name) {
            Some(prompt) => PromptSchema::Resolved(prompt.arguments),
            None if pid.provider == switchboard_prompts::LOCAL_PROVIDER
                || pid.provider == switchboard_prompts::BUILTIN_PROVIDER =>
            {
                PromptSchema::Missing
            }
            None => PromptSchema::Unresolved,
        },
        Err(_) => PromptSchema::Malformed,
    }
}

/// Every top-level `send` that hardcodes a prompt, as `(prompt_id,
/// template_var_keys)`. A top-level scan is complete for an invocable workflow: a
/// prompt send nested in a `for_each` is unreachable without that `for_each`,
/// which gates the workflow non-invocable (mirrors `Workflow::gated_step_kind`).
fn hardcoded_prompt_sends(workflow: &Workflow) -> Vec<(&str, Vec<&str>)> {
    workflow
        .steps
        .iter()
        .filter_map(|labeled| match &labeled.step {
            Step::Send(send) => send.prompt.as_deref().map(|id| {
                let keys = send.template_vars.iter().map(|(k, _)| k.as_str()).collect();
                (id, keys)
            }),
            _ => None,
        })
        .collect()
}

/// A user-fillable prompt argument surfaced in the form — the `A \ T` set (a
/// declared prompt argument with no `template_vars` binding), merged across
/// prompts by name.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct DerivedArgInfo {
    pub name: String,
    pub required: bool,
    pub description: Option<String>,
    /// The hardcoded prompt id(s) this field feeds — a light "which prompt" label;
    /// more than one when two prompts share a same-named argument.
    pub prompts: Vec<String>,
}

/// A binding/collision problem that blocks invocation.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct BindingIssue {
    /// The prompt id(s) involved; empty `argument` means the whole id is the
    /// problem (malformed).
    pub prompt: String,
    pub argument: String,
    pub reason: String,
}

/// Whether a workflow's hardcoded prompts are runnable as picked.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum FormCompatibility {
    /// Every prompt resolved and every binding is valid.
    Ok,
    /// A prompt drifted: an invalid binding (`template_vars` targets a missing
    /// argument), a malformed id, or a disallowed collision. Blocks Run.
    Incompatible { issues: Vec<BindingIssue> },
    /// One or more prompts aren't resolvable yet (cold MCP cache). Pending, not an
    /// error: the form shows a "resolving" affordance and re-fetches on sync.
    Unresolved { prompts: Vec<String> },
}

/// The complete invocation form for a picked workflow: declared inputs plus the
/// auto-derived user-fillable prompt-argument fields, plus a compatibility
/// verdict. Resolved per-pick (not in `list`), so a cold prompt cache costs one
/// workflow's resolution, not every workflow's on every menu render.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct WorkflowFormDescriptor {
    pub name: String,
    pub description: Option<String>,
    pub is_builtin: bool,
    pub invocable: bool,
    pub inputs: Vec<WorkflowInputInfo>,
    pub derived_args: Vec<DerivedArgInfo>,
    pub compatibility: FormCompatibility,
    /// The workflow's steps with **declared** recipients (slots unresolved), for
    /// the composer preview — the frontend resolves slots live against the form's
    /// bindings as the user assigns agents.
    pub steps: Vec<WorkflowStepInfo>,
}

/// The resolved form: the user-fillable derived fields, the compatibility
/// verdict, and the declared `text` inputs whose shadowed prompt argument is
/// **required**. The last is load-bearing for soundness: a `text?` input may
/// shadow a required prompt arg, and without enforcing it non-blank the workflow
/// would pass pre-flight and then fail at prompt render (the renderer rejects a
/// missing required arg). Both the descriptor and invoke use these together.
struct ResolvedForm {
    derived_args: Vec<DerivedArgInfo>,
    compatibility: FormCompatibility,
    /// Names of declared text inputs that feed a required prompt argument — must
    /// be enforced non-blank even when the input was declared optional.
    required_shadows: Vec<String>,
}

/// Merge one user-fillable arg into the by-name namespace: required if *either*
/// prompt requires it; first non-empty description wins; record every prompt it
/// feeds.
fn merge_derived(
    derived: &mut BTreeMap<String, DerivedArgInfo>,
    prompt_id: &str,
    arg: &PromptArgument,
) {
    derived
        .entry(arg.name.clone())
        .and_modify(|existing| {
            existing.required |= arg.required;
            if existing.description.is_none() {
                existing.description.clone_from(&arg.description);
            }
            if !existing.prompts.iter().any(|p| p == prompt_id) {
                existing.prompts.push(prompt_id.to_owned());
                // Symmetric to the text-shadow log in `classify_form`: a share is
                // surfaced (here a dev-log, plus `DerivedArgInfo.prompts` in the
                // descriptor) so it's diagnosable rather than silent.
                tracing::debug!(
                    arg = %arg.name,
                    prompt = prompt_id,
                    "workflow form: prompt argument shared across multiple prompts"
                );
            }
        })
        .or_insert_with(|| DerivedArgInfo {
            name: arg.name.clone(),
            required: arg.required,
            description: arg.description.clone(),
            prompts: vec![prompt_id.to_owned()],
        });
}

/// Classify every hardcoded prompt's bindings into the form's derived fields plus
/// a compatibility verdict, applying the merge-by-name + type-aware-collision
/// rules. Shared by the descriptor command and invoke pre-flight so they never
/// disagree. Binding classification per `send` (prompt args `A`, `template_vars`
/// keys `T`): computed `A ∩ T` is hidden; invalid `T \ A` blocks; user-fillable
/// `A \ T` becomes a field. Collisions with a declared input: a `text` input
/// shadows-and-satisfies (one field, its value also feeds the prompt); a non-text
/// input is the *only* collision that is an error.
fn classify_form(workflow: &Workflow, prompts: &PromptService) -> ResolvedForm {
    let declared: BTreeMap<&str, InputType> = workflow
        .inputs
        .iter()
        .map(|i| (i.name.as_str(), i.ty))
        .collect();

    let mut derived: BTreeMap<String, DerivedArgInfo> = BTreeMap::new();
    let mut issues: Vec<BindingIssue> = Vec::new();
    let mut unresolved: Vec<String> = Vec::new();

    for (id, tvar_keys) in hardcoded_prompt_sends(workflow) {
        match resolve_prompt_schema(prompts, id) {
            PromptSchema::Malformed => issues.push(BindingIssue {
                prompt: id.to_owned(),
                argument: String::new(),
                reason: format!("`{id}` is not a valid prompt id"),
            }),
            // Definitively missing (local/builtin): a blocking error shown at once,
            // not a pending "resolving" state — no sync will make it appear.
            PromptSchema::Missing => issues.push(BindingIssue {
                prompt: id.to_owned(),
                argument: String::new(),
                reason: format!("prompt `{id}` not found"),
            }),
            PromptSchema::Unresolved => unresolved.push(id.to_owned()),
            PromptSchema::Resolved(args) => {
                let arg_names: HashSet<&str> = args.iter().map(|a| a.name.as_str()).collect();
                // Invalid bindings: T \ A — a template_vars key the prompt has no
                // argument for. Any one is a blocking incompatibility (drift).
                for key in &tvar_keys {
                    if !arg_names.contains(key) {
                        issues.push(BindingIssue {
                            prompt: id.to_owned(),
                            argument: (*key).to_owned(),
                            reason: format!(
                                "prompt `{id}` has no argument `{key}` (the workflow binds it via template_vars)"
                            ),
                        });
                    }
                }
                // User-fillable: A \ T (skip computed A ∩ T), merged by name.
                for arg in &args {
                    if tvar_keys.contains(&arg.name.as_str()) {
                        continue;
                    }
                    merge_derived(&mut derived, id, arg);
                }
            }
        }
    }

    // Type-aware collisions with declared inputs.
    let mut surviving = Vec::new();
    let mut required_shadows = Vec::new();
    for (name, info) in derived {
        match declared.get(name.as_str()) {
            // text/text? input shadows-and-satisfies: the declared input is the
            // one field and its value also feeds the prompt; drop the duplicate.
            // If the shadowed prompt arg is required, the declared input must be
            // enforced non-blank (even if declared `text?`) so pre-flight stays
            // sound — otherwise the run would start and fail at prompt render.
            Some(InputType::Text) => {
                if info.required {
                    required_shadows.push(name.clone());
                }
                tracing::debug!(
                    arg = %name,
                    required = info.required,
                    "workflow form: text input feeds the prompt argument of the same name"
                );
            }
            // A non-text input feeding a string prompt arg is the only error case.
            Some(InputType::Agent | InputType::AgentList | InputType::TextList) => {
                issues.push(BindingIssue {
                    prompt: info.prompts.join(", "),
                    argument: name.clone(),
                    reason: format!(
                        "declared input `{name}` collides with a string prompt argument of the same name"
                    ),
                });
            }
            None => surviving.push(info),
        }
    }

    let compatibility = if !issues.is_empty() {
        FormCompatibility::Incompatible { issues }
    } else if unresolved.is_empty() {
        FormCompatibility::Ok
    } else {
        unresolved.sort();
        unresolved.dedup();
        FormCompatibility::Unresolved {
            prompts: unresolved,
        }
    };
    ResolvedForm {
        derived_args: surviving,
        compatibility,
        required_shadows,
    }
}

/// Enforce that every declared input shadowing a required prompt argument has a
/// non-blank bound value (defaults applied). Without this a `text?` shadowing a
/// required arg would pass pre-flight and fail at render. Run after
/// `bind_invocation` so an input's explicit non-blank `default` satisfies it.
fn enforce_required_shadows(
    required_shadows: &[String],
    bound: &BTreeMap<String, ScopeValue>,
) -> Result<(), AppError> {
    for name in required_shadows {
        let filled = matches!(bound.get(name), Some(ScopeValue::Text(s)) if !s.trim().is_empty());
        if !filled {
            return Err(invocation_msg(format!(
                "input {name:?} feeds a required prompt argument and must not be left blank"
            ))
            .into());
        }
    }
    Ok(())
}

/// Split a flat invocation payload into declared-input values and derived
/// prompt-arg values. The two are validated by different paths and conflating
/// them is a correctness regression: `validate_invocation` rejects any key it
/// doesn't declare (so a derived `context` routed through it would be refused),
/// and the derived path must not bypass the required/roster/`[agent]` checks the
/// declared path owns.
fn partition_payload(
    workflow: &Workflow,
    supplied: &BTreeMap<String, InputValue>,
) -> (BTreeMap<String, InputValue>, BTreeMap<String, InputValue>) {
    let declared: HashSet<&str> = workflow.inputs.iter().map(|i| i.name.as_str()).collect();
    let mut declared_vals = BTreeMap::new();
    let mut derived_vals = BTreeMap::new();
    for (k, v) in supplied {
        if declared.contains(k.as_str()) {
            declared_vals.insert(k.clone(), v.clone());
        } else {
            derived_vals.insert(k.clone(), v.clone());
        }
    }
    (declared_vals, derived_vals)
}

/// Validate the derived prompt-arg side of an invocation against the resolved
/// form, returning the derived values as plain strings (the runtime arg map).
/// Blocks an incompatible or not-yet-resolved workflow, rejects an unknown derived
/// key, and enforces required derived args.
fn validate_derived_args(
    derived_args: &[DerivedArgInfo],
    compatibility: &FormCompatibility,
    supplied: &BTreeMap<String, InputValue>,
) -> Result<BTreeMap<String, String>, AppError> {
    match compatibility {
        FormCompatibility::Incompatible { issues } => {
            let detail = issues
                .iter()
                .map(|i| i.reason.clone())
                .collect::<Vec<_>>()
                .join("; ");
            return Err(invocation_msg(format!("workflow cannot run: {detail}")).into());
        }
        FormCompatibility::Unresolved { prompts } => {
            return Err(invocation_msg(format!(
                "workflow prompt(s) not yet available: {} — try again after prompts sync",
                prompts.join(", ")
            ))
            .into());
        }
        FormCompatibility::Ok => {}
    }

    let known: HashSet<&str> = derived_args.iter().map(|d| d.name.as_str()).collect();
    let mut values = BTreeMap::new();
    for (k, v) in supplied {
        if !known.contains(k.as_str()) {
            return Err(invocation_msg(format!(
                "supplied prompt argument {k:?} is not a fillable argument of this workflow's prompts"
            ))
            .into());
        }
        match v {
            InputValue::Text(s) => values.insert(k.clone(), s.clone()),
            InputValue::List(_) => {
                return Err(invocation_msg(format!(
                    "prompt argument {k:?} must be a single value, not a list"
                ))
                .into());
            }
        };
    }
    for arg in derived_args {
        if arg.required && values.get(&arg.name).is_none_or(|s| s.trim().is_empty()) {
            return Err(invocation_msg(format!(
                "required prompt argument {:?} was not provided",
                arg.name
            ))
            .into());
        }
    }
    Ok(values)
}

/// Resolve a workflow's invocation form: declared inputs + auto-derived
/// user-fillable prompt-argument fields + a compatibility verdict. Resolves the
/// hardcoded prompts on demand (not in `list`); needs no `project_id` because
/// prompts are user-global. The frontend re-fetches on `prompts:synced` so a cold
/// MCP cache resolves once sync lands.
pub fn describe_workflow_form_impl(
    state: &AppState,
    name: &str,
    is_builtin: bool,
) -> Result<WorkflowFormDescriptor, AppError> {
    let workflow = snapshot_workflow(state, name, is_builtin)?;
    let invocable = workflow.gated_step_kind().is_none();
    let form = classify_form(&workflow, &state.prompts);
    // A declared input that feeds a *required* prompt arg is effectively required,
    // even if declared `text?` — report it as required so the form demands it.
    let mut inputs = input_infos(&workflow);
    for info in &mut inputs {
        if form.required_shadows.contains(&info.name) {
            info.optional = false;
        }
    }
    Ok(WorkflowFormDescriptor {
        name: workflow.name.clone(),
        description: Some(workflow.description.clone()),
        is_builtin,
        invocable,
        inputs,
        derived_args: form.derived_args,
        compatibility: form.compatibility,
        steps: step_display(&workflow),
    })
}

/// Validate a workflow invocation: capability gate + partitioned invocation rules
/// (declared inputs via `validate_invocation`; derived prompt args via the
/// classification + required-fill check, re-resolved here so invoke is authoritative).
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

    let (declared, derived) = partition_payload(&workflow, inputs);
    // Bind (not just validate) the declared inputs so defaults are applied before
    // the required-shadow check sees them.
    let bound = bind_invocation(&workflow, &declared, &names)?;
    let form = classify_form(&workflow, &state.prompts);
    validate_derived_args(&form.derived_args, &form.compatibility, &derived)?;
    enforce_required_shadows(&form.required_shadows, &bound)?;
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

    // Partition the flat payload: declared inputs go through `validate_invocation`
    // + `bind_invocation`; derived prompt args through the classification +
    // required-fill check. Invoke is authoritative — it re-resolves the prompt
    // schemas here (a prompt can change between form-open and invoke).
    let (declared, derived) = partition_payload(&workflow, inputs);
    let bound = bind_invocation(&workflow, &declared, &names)?;
    let form = classify_form(&workflow, &state.prompts);
    let derived_values = validate_derived_args(&form.derived_args, &form.compatibility, &derived)?;
    enforce_required_shadows(&form.required_shadows, &bound)?;

    // The run's user-fillable prompt-arg values: the derived args, plus every
    // declared `text` input (the shadow-and-satisfy escape hatch — a text input
    // may feed a prompt argument of the same name). Kept separate from the
    // workflow template scope so a prompt arg can't shadow a `text:` template var.
    let mut user_args: BTreeMap<String, String> = bound
        .iter()
        .filter_map(|(k, v)| match v {
            ScopeValue::Text(s) => Some((k.clone(), s.clone())),
            ScopeValue::List(_) => None,
        })
        .collect();
    user_args.extend(derived_values);

    let agents: BTreeMap<String, AgentId> = roster
        .iter()
        .map(|r| (canonicalize_for_uniqueness(&r.name), r.id))
        .collect();

    let provider = Arc::new(ProjectDispatchFactoryProvider::new(
        state, &project, &roster,
    ));

    let run_id = Uuid::now_v7();
    let run_path = project.run_path(run_id);
    prepare_run_dir(&project)?;
    let total_steps = workflow.steps.len();
    let workflow_name = workflow.name.clone();
    let cancel = CancellationToken::new();
    let done = Arc::new(tokio::sync::Notify::new());

    // Live-view step snapshot with recipients resolved to the bound agent names.
    let resolved_steps = resolve_step_display(&step_display(&workflow), &bound);

    let sink = Arc::new(ChannelProgressSink::new(
        run_id,
        project_id,
        workflow_name.clone(),
        total_steps,
        Arc::clone(&state.emitter),
        Arc::clone(&state.workflow_runs),
    ));

    // A held failed/interrupted run occupies the project too: it replaces the
    // compose box until dismissed, so launching requires dismissing it first.
    if project_has_held_run(state, &project, project_id) {
        return Err(AppError::WorkflowRunRequiresDismissal { project_id });
    }

    // Register the live run before spawning so cancel/list see it immediately.
    // Enforce one run per project **atomically** under the registry lock: check for
    // an existing active run for this project and insert under the same acquisition,
    // so two concurrent invokes can't both pass. The task is spawned only after.
    {
        let mut runs = lock(&state.workflow_runs);
        if runs.values().any(|r| r.project_id == project_id) {
            return Err(AppError::WorkflowAlreadyRunning { project_id });
        }
        runs.insert(
            run_id,
            ActiveRun {
                cancel: cancel.clone(),
                project_id,
                workflow: workflow_name.clone(),
                snapshot: RunSnapshot {
                    total_steps,
                    current_step: 0,
                },
                steps: resolved_steps,
                done: Arc::clone(&done),
            },
        );
    }

    let run = WorkflowRun {
        workflow,
        inputs: bound,
        user_args,
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
/// Whether the project has a retained **failed/interrupted** run awaiting
/// dismissal — which occupies the project the same way a live run does. Scanned
/// **outside** the registry lock by the caller (the lock is only taken briefly to
/// read live ids): `read_run_files` does blocking I/O and `workflow_runs` is a
/// sync mutex. Live runs are excluded because their not-yet-terminal file would
/// otherwise classify as `interrupted`.
/// Create a project's run-record directory before launching a run. `append_jsonl`
/// creates the file but not its parent, so without this every record write fails
/// (silently, to a warning) and a failed/interrupted run never survives a restart.
/// Fail loudly here rather than start a run whose records can't persist.
fn prepare_run_dir(project: &Project) -> Result<(), AppError> {
    let runs_dir = project.runs_dir();
    std::fs::create_dir_all(&runs_dir).map_err(|source| AppError::WorkflowRunSetupIo {
        path: runs_dir,
        source,
    })
}

fn project_has_held_run(state: &AppState, project: &Project, project_id: ProjectId) -> bool {
    let live_ids: Vec<Uuid> = {
        let runs = lock(&state.workflow_runs);
        runs.iter()
            .filter(|(_, r)| r.project_id == project_id)
            .map(|(id, _)| *id)
            .collect()
    };
    read_run_files(&project.runs_dir())
        .into_iter()
        .any(|(id, records)| {
            !live_ids.contains(&id)
                && classify_run_file(id, &records)
                    .is_some_and(|info| matches!(info.status, "failed" | "interrupted"))
        })
}

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
                steps: run.steps.clone(),
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
    let mut steps: Vec<WorkflowStepInfo> = Vec::new();
    let mut terminal: Option<(TerminalStatus, Option<usize>, Option<String>)> = None;
    for record in records {
        match record {
            RunRecord::Started {
                workflow: name,
                total_steps,
                steps: snapshot,
                ..
            } => {
                workflow.clone_from(name);
                total = *total_steps;
                steps.clone_from(snapshot);
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
            steps,
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
            steps,
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
    use switchboard_workflow::{TerminalStatus, WorkflowStepKind};

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
                steps: Vec::new(),
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

    fn slot(input: &str) -> RecipientRef {
        RecipientRef::Slot {
            input: input.to_owned(),
        }
    }
    fn lit(name: &str) -> RecipientRef {
        RecipientRef::Literal {
            name: name.to_owned(),
        }
    }
    fn step(label: &str, recipients: Vec<RecipientRef>) -> WorkflowStepInfo {
        WorkflowStepInfo {
            kind: WorkflowStepKind::Send,
            label: label.to_owned(),
            description: None,
            prompt: None,
            recipients,
            feeds_from: Vec::new(),
        }
    }

    #[test]
    fn resolve_step_display_expands_slots_to_bound_agent_names() {
        let declared = vec![
            step("Fan out", vec![slot("reviewers"), lit("ops")]),
            step("Hand off", vec![slot("worker")]),
            step("Unbound", vec![slot("missing")]),
        ];
        let mut bound = BTreeMap::new();
        bound.insert(
            "reviewers".to_owned(),
            ScopeValue::List(vec!["alice".to_owned(), "bob".to_owned()]),
        );
        bound.insert("worker".to_owned(), ScopeValue::Text("carol".to_owned()));

        let resolved = resolve_step_display(&declared, &bound);
        // `[agent]` slot → one literal per bound name; the literal passes through.
        assert_eq!(
            resolved[0].recipients,
            vec![lit("alice"), lit("bob"), lit("ops")]
        );
        // Single `agent` slot → one literal.
        assert_eq!(resolved[1].recipients, vec![lit("carol")]);
        // An unbound slot is left as-is (declared fallback).
        assert_eq!(resolved[2].recipients, vec![slot("missing")]);
        // Resolution preserves each step's kind (only recipients are rewritten).
        assert!(resolved.iter().all(|s| s.kind == WorkflowStepKind::Send));
    }

    #[test]
    fn classify_run_file_reconstructs_steps_for_failed_and_interrupted() {
        let snapshot = vec![step("Send the review", vec![slot("reviewers")])];
        let started = RunRecord::Started {
            workflow: "w".to_owned(),
            total_steps: 1,
            steps: snapshot.clone(),
            at: chrono::Utc::now(),
        };

        // Failed: terminal present.
        let failed = classify_run_file(
            Uuid::now_v7(),
            &[
                started.clone(),
                RunRecord::Terminal {
                    status: TerminalStatus::Failed,
                    failed_step: Some(0),
                    reason: Some("boom".to_owned()),
                    at: chrono::Utc::now(),
                },
            ],
        )
        .expect("failed run surfaces");
        assert_eq!(failed.status, "failed");
        assert_eq!(failed.steps, snapshot);

        // Interrupted: no terminal record. Same declared snapshot is reconstructed.
        let interrupted =
            classify_run_file(Uuid::now_v7(), &[started]).expect("interrupted run surfaces");
        assert_eq!(interrupted.status, "interrupted");
        assert_eq!(interrupted.steps, snapshot);
    }
}
