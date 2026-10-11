//! The closed set of codes a program acts on, and the total mapping from
//! [`AppError`] onto it.
//!
//! Spellings shared with the remote-control plan keep its spelling:
//! `unknown_recipient` is a send or forward recipient not in the roster;
//! `agent_busy_elsewhere` is the harness session held by another process (a
//! terminal resume, another Switchboard build). An unmapped variant is
//! `internal`, so adding an `AppError` variant can never produce an unknown
//! code on the wire — only a less specific one.

use switchboard_core::CoreError;
use switchboard_prompts::PromptError;

use crate::error::AppError;

pub const PROJECT_NOT_FOUND: &str = "project_not_found";
pub const PROJECT_NOT_LOADED: &str = "project_not_loaded";
pub const AGENT_NOT_FOUND: &str = "agent_not_found";
pub const UNKNOWN_RECIPIENT: &str = "unknown_recipient";
pub const AGENT_BUSY: &str = "agent_busy";
pub const AGENT_BUSY_ELSEWHERE: &str = "agent_busy_elsewhere";
pub const SOURCE_BUSY: &str = "source_busy";
pub const DIRECTORY_MISSING: &str = "directory_missing";
pub const DESKTOP_UNAVAILABLE: &str = "desktop_unavailable";
pub const PROJECT_LOAD_TIMEOUT: &str = "project_load_timeout";
pub const PROJECT_LOCKED: &str = "project_locked";
pub const PROMPT_NEEDS_SIGN_IN: &str = "prompt_needs_sign_in";
pub const PROMPT_TOO_LARGE: &str = "prompt_too_large";
pub const SEND_UNKNOWN: &str = "send_unknown";
pub const RUN_UNKNOWN: &str = "run_unknown";
pub const WORKFLOW_RUN_REQUIRES_DISMISSAL: &str = "workflow_run_requires_dismissal";
pub const WORKFLOW_ALREADY_RUNNING: &str = "workflow_already_running";
pub const RESULT_TOO_LARGE: &str = "result_too_large";
pub const REQUEST_IN_PROGRESS: &str = "request_in_progress";
pub const INVALID_ARGUMENT: &str = "invalid_argument";
pub const INTERNAL: &str = "internal";

/// The wire code for an error. Total: every variant maps, the unknown ones to
/// [`INTERNAL`].
#[must_use]
pub fn error_code(error: &AppError) -> &'static str {
    match error {
        AppError::Core(core) => core_code(core),
        AppError::Prompt(prompt) => prompt_code(prompt),
        AppError::ProjectNotLoaded(_)
        | AppError::ProjectUnderMaintenance(_)
        | AppError::ProjectViewStale(_) => PROJECT_NOT_LOADED,
        AppError::ProjectLocked(_) => PROJECT_LOCKED,
        AppError::AgentNotFound(_) | AppError::ForwardSourceMoved { .. } => AGENT_NOT_FOUND,
        AppError::UnknownRecipient { .. } => UNKNOWN_RECIPIENT,
        AppError::AgentBusy | AppError::RecipientsBusy { .. } => AGENT_BUSY,
        AppError::SessionInUse => AGENT_BUSY_ELSEWHERE,
        AppError::SourceBusy { .. } => SOURCE_BUSY,
        AppError::DirectoryMissing { .. } => DIRECTORY_MISSING,
        AppError::DesktopUnavailable { .. } => DESKTOP_UNAVAILABLE,
        AppError::ProjectLoadTimeout(_) => PROJECT_LOAD_TIMEOUT,
        AppError::PromptTooLarge(_) => PROMPT_TOO_LARGE,
        AppError::SendUnknown(_) => SEND_UNKNOWN,
        AppError::WorkflowRunNotFound { .. } => RUN_UNKNOWN,
        AppError::WorkflowRunRequiresDismissal { .. } => WORKFLOW_RUN_REQUIRES_DISMISSAL,
        AppError::WorkflowAlreadyRunning { .. } => WORKFLOW_ALREADY_RUNNING,
        AppError::ResultTooLarge { .. } => RESULT_TOO_LARGE,
        AppError::RequestInProgress { .. } => REQUEST_IN_PROGRESS,
        AppError::Joined(original) => error_code(original),
        AppError::InvalidArgument(_)
        | AppError::InvalidUuid { .. }
        | AppError::NoForwardSources
        | AppError::Workflow(_)
        | AppError::WorkflowNotFound { .. }
        | AppError::WorkflowStepUnsupported
        | AppError::WorkflowRunActive { .. }
        | AppError::ForkSourceBusy { .. }
        | AppError::ForkSourceHasNoSession { .. } => INVALID_ARGUMENT,
        _ => INTERNAL,
    }
}

fn core_code(error: &CoreError) -> &'static str {
    match error {
        CoreError::ProjectNotFound(_) => PROJECT_NOT_FOUND,
        CoreError::AgentNotFound(_) => AGENT_NOT_FOUND,
        CoreError::NotADirectory { .. } => DIRECTORY_MISSING,
        CoreError::InvalidName { .. }
        | CoreError::DuplicateAgentName { .. }
        | CoreError::DuplicateProjectName { .. }
        | CoreError::SelectionUnsupported { .. }
        | CoreError::EffortWithoutModel { .. }
        | CoreError::SessionForkUnsupported { .. } => INVALID_ARGUMENT,
        _ => INTERNAL,
    }
}

fn prompt_code(error: &PromptError) -> &'static str {
    match error {
        PromptError::McpNeedsAuth { .. } => PROMPT_NEEDS_SIGN_IN,
        PromptError::MalformedId { .. }
        | PromptError::ProviderNotFound { .. }
        | PromptError::PromptNotFound { .. }
        | PromptError::MissingRequiredArgument { .. }
        | PromptError::UnknownArgument { .. }
        | PromptError::Render { .. } => INVALID_ARGUMENT,
        _ => INTERNAL,
    }
}
