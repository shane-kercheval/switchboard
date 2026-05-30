use std::path::PathBuf;

pub type Result<T> = std::result::Result<T, CoreError>;

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CoreError {
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("path is not a directory: {path}")]
    NotADirectory { path: PathBuf },

    #[error("invalid name {name:?}: must match `^[A-Za-z0-9_-]+$` and be non-empty")]
    InvalidName { name: String },

    #[error("agent name {name:?} already exists in this project (collides with {existing:?})")]
    DuplicateAgentName { name: String, existing: String },

    #[error("project name {name:?} already exists in this directory (collides with {existing:?})")]
    DuplicateProjectName { name: String, existing: String },

    #[error("project not found: {0}")]
    ProjectNotFound(uuid::Uuid),

    #[error("agent not found: {0}")]
    AgentNotFound(uuid::Uuid),

    #[error("unsupported config version at {path}: found {found}, expected {expected}")]
    UnsupportedConfigVersion {
        path: PathBuf,
        found: u32,
        expected: u32,
    },

    #[error("corrupt JSONL at {path} (line {line_number}): {source}\n  line: {line}")]
    CorruptJsonl {
        path: PathBuf,
        line_number: usize,
        line: String,
        #[source]
        source: serde_json::Error,
    },

    #[error("corrupt YAML at {path}: {source}")]
    CorruptYaml {
        path: PathBuf,
        #[source]
        source: serde_norway::Error,
    },

    #[error("failed to serialize value for {path}: {source}")]
    Serialize {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error("expected append-only file is missing after init: {path}")]
    MissingAppendOnlyFile { path: PathBuf },
}

impl CoreError {
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}
