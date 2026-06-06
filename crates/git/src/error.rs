use std::path::PathBuf;

pub type Result<T> = std::result::Result<T, GitError>;

/// Errors from the git read layer.
///
/// The distinction that matters: "this path isn't a git repo" and "this path is
/// unavailable" are **not** errors — they are expected, non-error UI states
/// represented in [`crate::RepoView::available`] (a caller asks for a repo view
/// and gets back `available: false`). `GitError` is reserved for *genuine*
/// failures: a corrupt repository, or an I/O failure encountered partway through
/// an otherwise-valid read. Those are the cases a caller cannot render as a
/// normal tree and must surface as an error.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum GitError {
    /// A `libgit2` operation failed partway through reading a repo we had
    /// already opened — e.g. a corrupt object, a broken ref, an I/O error mid
    /// walk. Carries the repo root for context.
    #[error("git read failed for {root}: {source}")]
    Read {
        root: PathBuf,
        #[source]
        source: git2::Error,
    },
}

impl GitError {
    pub(crate) fn read(root: impl Into<PathBuf>, source: git2::Error) -> Self {
        Self::Read {
            root: root.into(),
            source,
        }
    }
}
