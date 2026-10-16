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
pub const COMPACTION_UNSUPPORTED: &str = "compaction_unsupported";
pub const NO_SESSION: &str = "no_session";
pub const FORK_NOT_MATERIALIZED: &str = "fork_not_materialized";
pub const FORK_UNSUPPORTED: &str = "fork_unsupported";
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
        AppError::AgentNotFound(_)
        | AppError::AgentNotInProject { .. }
        | AppError::ForwardSourceMoved { .. } => AGENT_NOT_FOUND,
        AppError::UnknownRecipient { .. } => UNKNOWN_RECIPIENT,
        AppError::AgentBusy
        | AppError::RecipientsBusy { .. }
        | AppError::RemoveAgentBusy { .. }
        | AppError::RemoveAgentWorkflowRunning { .. } => AGENT_BUSY,
        AppError::SessionInUse => AGENT_BUSY_ELSEWHERE,
        // A fork's source mid-turn, including the Codex case where another
        // program holds the parent's session: the advice is the same, wait for
        // the source.
        AppError::SourceBusy { .. } | AppError::ForkSourceBusy { .. } => SOURCE_BUSY,
        AppError::DirectoryMissing { .. } => DIRECTORY_MISSING,
        AppError::DesktopUnavailable { .. } | AppError::DesktopSyncFailed { .. } => {
            DESKTOP_UNAVAILABLE
        }
        AppError::ProjectLoadTimeout(_) => PROJECT_LOAD_TIMEOUT,
        AppError::PromptTooLarge(_) => PROMPT_TOO_LARGE,
        AppError::SendUnknown(_) | AppError::SendNotCancellable { .. } => SEND_UNKNOWN,
        AppError::WorkflowRunNotFound { .. } => RUN_UNKNOWN,
        AppError::WorkflowRunRequiresDismissal { .. } => WORKFLOW_RUN_REQUIRES_DISMISSAL,
        AppError::WorkflowAlreadyRunning { .. } => WORKFLOW_ALREADY_RUNNING,
        AppError::ResultTooLarge { .. } => RESULT_TOO_LARGE,
        AppError::RequestInProgress { .. } => REQUEST_IN_PROGRESS,
        AppError::CompactionUnsupported { .. } => COMPACTION_UNSUPPORTED,
        AppError::CompactionSourceHasNoSession { .. } | AppError::ForkSourceHasNoSession { .. } => {
            NO_SESSION
        }
        AppError::CompactionForkNotMaterialized { .. } => FORK_NOT_MATERIALIZED,
        AppError::Shared(original) => error_code(original),
        AppError::InvalidArgument(_)
        | AppError::InvalidUuid { .. }
        | AppError::NoForwardSources
        | AppError::Workflow(_)
        | AppError::WorkflowNotFound { .. }
        | AppError::WorkflowStepUnsupported
        | AppError::WorkflowRunActive { .. }
        | AppError::ReplyInUserWorkflow { .. }
        | AppError::RunStartedByUser { .. } => INVALID_ARGUMENT,
        // Includes `TimedOut`, `Interrupted`, and `Internal`: operational
        // failures, not the caller's input; their messages say to try again.
        _ => INTERNAL,
    }
}

fn core_code(error: &CoreError) -> &'static str {
    match error {
        CoreError::ProjectNotFound(_) => PROJECT_NOT_FOUND,
        CoreError::AgentNotFound(_) => AGENT_NOT_FOUND,
        CoreError::NotADirectory { .. } => DIRECTORY_MISSING,
        CoreError::SessionForkUnsupported { .. } => FORK_UNSUPPORTED,
        CoreError::InvalidName { .. }
        | CoreError::DuplicateAgentName { .. }
        | CoreError::DuplicateProjectName { .. }
        | CoreError::SelectionUnsupported { .. }
        | CoreError::EffortWithoutModel { .. } => INVALID_ARGUMENT,
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[test]
    fn a_variant_with_no_code_of_its_own_is_internal() {
        for error in [
            AppError::Internal("x".to_owned()),
            AppError::Interrupted,
            AppError::TimedOut { what: "x" },
            AppError::WorkflowsDirUnavailable,
            AppError::BranchComparisonStale,
        ] {
            assert_eq!(error_code(&error), INTERNAL, "{error:?}");
        }
    }

    #[test]
    fn the_fork_refusals_carry_their_own_codes() {
        use switchboard_core::{CoreError, HarnessKind};
        let name = || "alice".to_owned();
        assert_eq!(
            error_code(&AppError::ForkSourceHasNoSession { name: name() }),
            NO_SESSION
        );
        assert_eq!(
            error_code(&AppError::ForkSourceBusy { name: name() }),
            SOURCE_BUSY
        );
        assert_eq!(
            error_code(&AppError::Core(CoreError::SessionForkUnsupported {
                harness: HarnessKind::Antigravity
            })),
            FORK_UNSUPPORTED
        );
        assert_eq!(error_code(&AppError::CodexForkNeedsRealHarness), INTERNAL);
    }

    #[test]
    fn a_shared_error_carries_the_code_of_the_one_it_wraps() {
        let inner = AppError::UnknownRecipient {
            project_id: uuid::Uuid::now_v7(),
            agent_id: uuid::Uuid::now_v7(),
        };
        let shared = AppError::Shared(Arc::new(AppError::Shared(Arc::new(inner))));
        assert_eq!(error_code(&shared), UNKNOWN_RECIPIENT);
    }
}
