//! Generic filesystem helpers used by `directory` and `project`. JSONL is used
//! for the append-only logs (projects index, agent registry); YAML for the
//! human-editable config files. Lives in its own module so `directory.rs`
//! doesn't have to import from a sibling's `pub(crate)` surface.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::error::{CoreError, Result};

/// Append a single value to a JSONL file. Creates the file if it doesn't exist.
///
/// Serialization failures map to `CoreError::Serialize` — distinct from
/// `CoreError::CorruptJsonl`, which means "data already on disk is malformed."
///
/// **Durability — and the contract callers must honor.** After writing we
/// `sync_data()` the file (and, only when this call *created* the file, fsync
/// the parent directory so the new directory entry is durable). These logs are
/// append-only sources of truth; a torn or lost line from a power loss between
/// the userspace write and writeback bricks future reads (`read_jsonl` is
/// fail-loud — see `CorruptJsonl`), so the fsync is the crash-safety this layer
/// promises. A **sync failure is returned as an error** (it is the kernel
/// reporting that durability could not be confirmed — `flush` only reaches the
/// OS page cache, not stable storage), so the durability guarantee stays
/// enforceable for high-frequency callers that depend on it (the M4.2 journal).
///
/// **Critical caller contract:** because the sync happens *after* `writeln!`,
/// an `Err` from `append_jsonl` does **not** mean "the record was not written"
/// — the line may already be on disk and visible. So a caller must **never
/// destructively roll back** (delete the artifact the record refers to) on an
/// append error: doing so after a possible commit leaves a dangling reference.
/// Keep the artifact and surface the error (see `Directory::create_project`).
/// The parent-directory fsync has no portable Windows equivalent, so it's gated
/// to unix; Windows durability is deferred.
pub(crate) fn append_jsonl<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let mut line = serde_json::to_string(value).map_err(|source| CoreError::Serialize {
        path: path.to_owned(),
        source,
    })?;
    // Whether *this* call creates the file decides if the parent directory
    // entry needs syncing — appends to an existing file leave the entry
    // unchanged, so a dir fsync there is pure overhead (load-bearing for the
    // high-frequency logs that reuse this path: the M4.2 journal, M6 runs).
    let created = !path.exists();
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| CoreError::io(path, e))?;
    // Write the record and its newline terminator in a single `write_all` of
    // one buffer, rather than `writeln!` (which can issue the content and the
    // `\n` as separate syscalls). One write narrows the window in which a crash
    // mid-append could leave a torn, unterminated line. (It does not *eliminate*
    // torn lines — a single large write can still partially fail — so the read
    // path stays fail-loud on corruption; recovering a torn trailing line is a
    // separate hardening of `read_jsonl`.)
    line.push('\n');
    file.write_all(line.as_bytes())
        .map_err(|e| CoreError::io(path, e))?;
    file.flush().map_err(|e| CoreError::io(path, e))?;
    file.sync_data().map_err(|e| CoreError::io(path, e))?;
    #[cfg(unix)]
    if created && let Some(parent) = path.parent() {
        File::open(parent)
            .and_then(|dir| dir.sync_all())
            .map_err(|e| CoreError::io(parent, e))?;
    }
    Ok(())
}

/// Read every line of a JSONL file and parse it as `T`. Returns an empty `Vec`
/// if the file doesn't exist — callers that consider absence corruption (e.g.,
/// `Directory::list_projects` after init) must check existence themselves
/// before calling this.
pub(crate) fn read_jsonl<T: DeserializeOwned>(path: &Path) -> Result<Vec<T>> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(CoreError::io(path, e)),
    };
    let reader = BufReader::new(file);
    let mut out = Vec::new();
    for (idx, line) in reader.lines().enumerate() {
        let line = line.map_err(|e| CoreError::io(path, e))?;
        if line.trim().is_empty() {
            continue;
        }
        let parsed: T = serde_json::from_str(&line).map_err(|source| CoreError::CorruptJsonl {
            path: path.to_owned(),
            line_number: idx + 1,
            line: line.clone(),
            source,
        })?;
        out.push(parsed);
    }
    Ok(out)
}

/// Atomically rewrite a JSONL file with exactly `values`, replacing any
/// existing content: serialize all, write `<path>.tmp` in the same directory,
/// `rename` over the target.
///
/// **Why rewrite, not append.** The registry is otherwise an append-only log
/// (`append_jsonl`), but removing or renaming an agent edits a record in place,
/// which can't be expressed as an append without a compaction/tombstone story
/// we're not building. The agent count is tiny, so a full rewrite is cheap and
/// keeps the read path a simple `read_jsonl`. Adds still go through
/// `append_jsonl`; this is only for the in-place edits.
///
/// **Durability matches `append_jsonl`** (the registry's add path), so the two
/// halves of the same source-of-truth fail the same way: `sync_data` the tmp
/// before the rename (else a power loss after the rename reaches disk but the
/// data didn't leaves an empty/partial registry — every agent lost), and fsync
/// the parent directory after the rename (a rename is a directory-entry change,
/// not durable until the dir is synced — else the rewrite can silently revert on
/// power loss). Unlike `append_jsonl`'s create-only dir fsync, a
/// rewrite-over-existing *always* changes the entry, so the dir fsync is
/// unconditional. Unix-only (no portable Windows equivalent), matching
/// `append_jsonl`. This is a rare path (remove/rename), not the hot append, so
/// the extra syncs cost nothing meaningful.
pub(crate) fn write_jsonl<T: Serialize>(path: &Path, values: &[T]) -> Result<()> {
    let mut buf = String::new();
    for value in values {
        let line = serde_json::to_string(value).map_err(|source| CoreError::Serialize {
            path: path.to_owned(),
            source,
        })?;
        buf.push_str(&line);
        buf.push('\n');
    }
    let tmp = tmp_path(path);
    {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)
            .map_err(|e| CoreError::io(&tmp, e))?;
        file.write_all(buf.as_bytes())
            .map_err(|e| CoreError::io(&tmp, e))?;
        file.flush().map_err(|e| CoreError::io(&tmp, e))?;
        file.sync_data().map_err(|e| CoreError::io(&tmp, e))?;
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(CoreError::io(path, e));
    }
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        File::open(parent)
            .and_then(|dir| dir.sync_all())
            .map_err(|e| CoreError::io(parent, e))?;
    }
    Ok(())
}

pub fn read_yaml<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let bytes = std::fs::read(path).map_err(|e| CoreError::io(path, e))?;
    serde_norway::from_slice(&bytes).map_err(|source| CoreError::CorruptYaml {
        path: path.to_owned(),
        source,
    })
}

/// Atomic-ish YAML write: serialize, write to `<path>.tmp` in the *same
/// directory* as the target, then `rename` over the target. Same-filesystem
/// rename is atomic on POSIX/Windows; a cross-filesystem temp dir would
/// degrade to copy+delete and defeat the purpose, so we stay adjacent.
///
/// **Known durability gap (tracked follow-up).** Unlike `write_jsonl` /
/// `append_jsonl`, this does *not* `sync_data` the tmp before rename nor fsync
/// the parent dir after — so a power loss can leave a partial/stale file. Lower
/// frequency than the registry, but it persists `workspace.yaml` (the user's
/// whole cross-directory project list) and `config.yaml`, so the gap isn't
/// trivial. Hardening it the way `write_jsonl` does is a deliberate follow-up,
/// out of the milestone that added `write_jsonl`.
pub fn write_yaml<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let yaml = serde_norway::to_string(value).map_err(|source| CoreError::CorruptYaml {
        path: path.to_owned(),
        source,
    })?;

    let tmp = tmp_path(path);
    std::fs::write(&tmp, yaml).map_err(|e| CoreError::io(&tmp, e))?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(CoreError::io(path, e));
    }
    Ok(())
}

fn tmp_path(target: &Path) -> std::path::PathBuf {
    let mut name = target
        .file_name()
        .map(std::ffi::OsString::from)
        .unwrap_or_default();
    name.push(".tmp");
    target
        .parent()
        .map_or_else(|| std::path::PathBuf::from(&name), |p| p.join(&name))
}
