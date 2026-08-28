//! A working directory, reduced to what dispatch actually needs.
//!
//! This used to own everything: the per-directory `projects.jsonl`, a
//! `projects/<id>/` tree, a `config.yaml`, and the create/rename/delete
//! lifecycle over all of it. All of that moved to the user-global [`crate::Store`],
//! which is why deleting a checkout no longer destroys the projects that lived
//! in it.
//!
//! What remains is the one thing a working directory is still *for*: canonical
//! path identity, and the check that a path is a directory that exists. A
//! project references a directory by `directory_id` through the store's catalog;
//! this type is what the boundary canonicalizes through, so a path arriving from
//! the frontend is identified the same way it was when it was catalogued.
//!
//! Deleted rather than deprecated: `init`, `has_switchboard`, the directory
//! `config.yaml`, and the whole project lifecycle. Nothing writes
//! `<directory>/.switchboard/` any more, so leaving read paths for it would mean
//! two layouts that could disagree — and the migration tool, not a fallback
//! read, is what carries existing users across.

use std::path::{Path, PathBuf};

use crate::error::{CoreError, Result};

/// A working directory an agent can be dispatched into: a canonical path that
/// exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Directory {
    pub path: PathBuf,
}

impl Directory {
    /// Wraps a path, canonicalizing symlinks and resolving to absolute. The path
    /// must exist and be a directory.
    ///
    /// **This is the identity boundary.** Canonicalizing here is what makes two
    /// spellings of one directory — a symlink, a `/./`, a relative path — resolve
    /// to the same catalog row, and therefore to the same `directory_id`. The
    /// store's `add_directory` canonicalizes the same way for the same reason.
    pub fn at(path: &Path) -> Result<Directory> {
        let canonical = std::fs::canonicalize(path).map_err(|e| CoreError::io(path, e))?;
        if !canonical.is_dir() {
            return Err(CoreError::NotADirectory { path: canonical });
        }
        Ok(Directory { path: canonical })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn at_requires_existing_directory() {
        let tmp = TempDir::new().unwrap();
        let missing = tmp.path().join("nope");
        assert!(Directory::at(&missing).is_err());
    }

    #[test]
    fn at_rejects_file_paths() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("a.txt");
        std::fs::write(&file, "x").unwrap();
        assert!(matches!(
            Directory::at(&file).unwrap_err(),
            CoreError::NotADirectory { .. }
        ));
    }

    #[test]
    fn at_canonicalizes_symlinks() {
        // The identity boundary: a symlinked spelling must resolve to the same
        // path the store catalogued, or one directory acquires two identities.
        let tmp = TempDir::new().unwrap();
        let real = tmp.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        assert_eq!(
            Directory::at(&link).unwrap().path,
            std::fs::canonicalize(&real).unwrap()
        );
    }
}
