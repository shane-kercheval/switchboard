//! Switchboard workflow language — the pure, runtime-free layer that turns a
//! workflow `.yaml` file into a validated, executable in-memory model and renders
//! its templates. No Tauri, no dispatcher, no prompt service, no async.
//!
//! `docs/workflow-spec.md` is the authoritative specification; this crate is its
//! implementation. Where the two disagree, the spec wins.
//!
//! ## Why this is a separate, pure crate
//!
//! The workflow *language* (file model, parser, parse-time + invocation-time
//! validation, the `MiniJinja`-subset template environment + the four helper
//! functions, and the run/checkpoint record types) is a large self-contained
//! body of logic with its own fixture-driven test surface — mirroring the
//! workspace's existing discipline where `core` / `harness` / `dispatcher` are
//! pure and Tauri-free. The interpreter that actually *runs* a workflow is a
//! conductor over app-owned machinery (the `Dispatcher`, `PromptService`,
//! transcript loading, checkpoint file IO, the event emitter), so it lives in
//! `crates/app`, not here. Keeping the language pure is what makes the spec's
//! three worked examples testable as fixtures with no app present.
//!
//! ## Output scope is resolved text, not turn ids
//!
//! The template helpers and the in-memory per-run output scope ([`OutputScope`])
//! hold each agent's **resolved completed-turn text**, captured from the live
//! event stream at completion — never a turn-id to be re-joined from a harness
//! later. The dispatcher's `turn_id` is not joinable to a harness file's own turn
//! ids, and one harness has no per-turn id at all, so that join is impossible;
//! capturing text at completion sidesteps identity entirely. (The spec's
//! §"Output scope" states this; an older parenthetical in §"Retry from inside a
//! `for_each`" still said "turn-id" — that is stale and is corrected in favor of
//! resolved text.)
//!
//! ## Known limitation: parse-time vs render-time
//!
//! Parsing validates that every template *parses* and stays within the subset,
//! but a template variable's *resolution* is checked only at render time (under
//! strict-undefined). A reference to a name that resolves in no scope therefore
//! surfaces as a [`WorkflowError::Render`] when the step executes, not at parse
//! or invocation — the interpreter must surface render errors at step
//! execution.

mod builtin;
mod error;
mod invocation;
mod model;
mod parse;
mod run;
mod template;

pub use builtin::{
    builtin_workflow, builtin_workflow_content, builtin_workflows, parse_builtin_workflows,
};
pub use error::{Result, WorkflowError};
pub use invocation::{InputValue, bind_invocation, validate_agent_list, validate_invocation};
pub use model::{
    ForEachStep, InputDecl, InputType, PauseForUserStep, SendStep, Step, Templated,
    UNSUPPORTED_STEP_MESSAGE, WaitForAllStep, WaitForStep, Workflow,
};
pub use parse::parse_workflow;
pub use run::{RunRecord, RunStatus, TerminalStatus};
pub use template::{OutputScope, Scope, ScopeValue, render, resolve_agent_refs, validate_template};
