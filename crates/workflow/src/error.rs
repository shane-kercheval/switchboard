//! Typed errors at the crate boundary (workspace `thiserror` convention).
//!
//! The variants partition by *when* and *why* a workflow is rejected, which is
//! also how `docs/workflow-spec.md` frames validation:
//!
//! - [`WorkflowError::Yaml`] — the file isn't well-formed YAML at all.
//! - [`WorkflowError::Validation`] — parse-time structural rules (top-level keys,
//!   step shape, reserved keys, input grammar, nesting, name collisions).
//! - [`WorkflowError::Template`] — a template string fails to parse or uses a
//!   feature outside the spec's `MiniJinja` subset (caught at parse time).
//! - [`WorkflowError::Render`] — a template fails *at render*: an undefined
//!   variable (strict-undefined) or a helper called for an agent with no output.
//! - [`WorkflowError::Invocation`] — invocation-time rules (missing required
//!   inputs, non-existent agents, unresolvable prompt ids, empty/duplicate agent
//!   lists).
//!
//! Each carries an actionable, human-readable `message`; consumers match on the
//! variant and surface the message.

pub type Result<T> = std::result::Result<T, WorkflowError>;

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum WorkflowError {
    #[error("workflow file is not valid YAML: {0}")]
    Yaml(String),

    #[error("invalid workflow: {message}")]
    Validation { message: String },

    #[error("invalid template in {field}: {message}")]
    Template { field: String, message: String },

    #[error("template render error: {message}")]
    Render { message: String },

    #[error("workflow invocation rejected: {message}")]
    Invocation { message: String },
}

impl WorkflowError {
    pub(crate) fn validation(message: impl Into<String>) -> Self {
        Self::Validation {
            message: message.into(),
        }
    }

    pub(crate) fn template(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Template {
            field: field.into(),
            message: message.into(),
        }
    }

    pub(crate) fn render(message: impl Into<String>) -> Self {
        Self::Render {
            message: message.into(),
        }
    }

    pub(crate) fn invocation(message: impl Into<String>) -> Self {
        Self::Invocation {
            message: message.into(),
        }
    }
}
