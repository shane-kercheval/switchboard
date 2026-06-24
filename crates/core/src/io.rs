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
/// enforceable for high-frequency callers that depend on it (the journal).
///
/// **Critical caller contract:** because the sync happens *after* `writeln!`,
/// an `Err` from `append_jsonl` does **not** mean "the record was not written"
/// — the line may already be on disk and visible. So a caller must **never
/// destructively roll back** (delete the artifact the record refers to) on an
/// append error: doing so after a possible commit leaves a dangling reference.
/// Keep the artifact and surface the error (see `Directory::create_project`).
/// The parent-directory fsync has no portable Windows equivalent, so it's gated
/// to unix; Windows durability is deferred.
pub fn append_jsonl<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let mut line = serde_json::to_string(value).map_err(|source| CoreError::Serialize {
        path: path.to_owned(),
        source,
    })?;
    // Whether *this* call creates the file decides if the parent directory
    // entry needs syncing — appends to an existing file leave the entry
    // unchanged, so a dir fsync there is pure overhead (load-bearing for the
    // high-frequency logs that reuse this path: the journal, runs).
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
pub fn read_jsonl<T: DeserializeOwned>(path: &Path) -> Result<Vec<T>> {
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

/// Atomic YAML write: serialize, write to `<path>.tmp` in the *same directory*
/// as the target, then `rename` over the target. Same-filesystem rename is
/// atomic on POSIX/Windows; a cross-filesystem temp dir would degrade to
/// copy+delete and defeat the purpose, so we stay adjacent.
///
/// **Durability matches `write_jsonl`** (see its doc for the full rationale):
/// `sync_data` the tmp before the rename (else a power loss after the rename
/// reaches disk but the data didn't leaves an empty/partial file), and fsync the
/// parent directory after the rename (a rename is a directory-entry change, not
/// durable until the dir is synced — else the write can silently revert on power
/// loss). This matters because `write_yaml` persists `workspace.yaml` (the user's
/// whole cross-directory project list) and the shared `config.yaml`. Unix-only
/// parent-dir fsync (no portable Windows equivalent), matching `write_jsonl`.
pub fn write_yaml<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let yaml = serde_norway::to_string(value).map_err(|source| CoreError::CorruptYaml {
        path: path.to_owned(),
        source,
    })?;

    let tmp = tmp_path(path);
    {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)
            .map_err(|e| CoreError::io(&tmp, e))?;
        file.write_all(yaml.as_bytes())
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

/// Serializes every `edit_yaml_mapping` call process-wide. A shared YAML config
/// (notably `config.yaml`, written by *both* the prompt providers and personal
/// preferences) is read-modify-written by independent subsystems; without one
/// gate their reads and writes interleave and silently drop each other's keys —
/// worsened by `write_yaml`'s fixed temp path, which two concurrent writes would
/// collide on. One process-wide lock is sufficient and correct here: the app is
/// single-process (the per-project instance lock), config writes are rare, and
/// only this helper edits the shared file. (Per-file locking would avoid
/// serializing edits to *different* files, but nothing needs that today.)
static YAML_EDIT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Read a YAML file as a top-level mapping, apply `edit`, and write it back —
/// atomic against other `edit_yaml_mapping` calls. Every key the closure doesn't
/// touch is preserved, so subsystems that share one file (each owning different
/// keys) never clobber each other's sections.
///
/// Missing / empty / `null` → a fresh mapping (first write seeds the file). A
/// file that parses to a non-mapping is refused with [`CoreError::NotAMapping`]
/// rather than overwritten. The closure is infallible by design: do any
/// fallible work (e.g. serializing the value to insert) *before* calling this, so
/// the lock is held only across the read-modify-write.
pub fn edit_yaml_mapping<F>(path: &Path, edit: F) -> Result<()>
where
    F: FnOnce(&mut serde_norway::Mapping),
{
    let _guard = YAML_EDIT_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut root = read_yaml_mapping(path)?;
    edit(&mut root);
    write_yaml(path, &serde_norway::Value::Mapping(root))
}

/// Read a YAML file as a top-level mapping for an in-place edit. Absent / empty /
/// `null` → a fresh mapping; a non-mapping or unparseable file is an error so a
/// caller never clobbers a config it can't safely round-trip.
fn read_yaml_mapping(path: &Path) -> Result<serde_norway::Mapping> {
    use serde_norway::Value;
    if !path.exists() {
        return Ok(serde_norway::Mapping::new());
    }
    let bytes = std::fs::read(path).map_err(|e| CoreError::io(path, e))?;
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(serde_norway::Mapping::new());
    }
    match serde_norway::from_slice::<Value>(&bytes) {
        Ok(Value::Mapping(mapping)) => Ok(mapping),
        Ok(Value::Null) => Ok(serde_norway::Mapping::new()),
        Ok(_) => Err(CoreError::NotAMapping {
            path: path.to_owned(),
        }),
        Err(source) => Err(CoreError::CorruptYaml {
            path: path.to_owned(),
            source,
        }),
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_norway::Value;
    use tempfile::tempdir;

    fn s(text: &str) -> Value {
        Value::String(text.to_owned())
    }

    #[test]
    fn write_yaml_round_trips_and_overwrites_without_leaving_a_tmp() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.yaml");

        let mut first = serde_norway::Mapping::new();
        first.insert(s("editor_command"), s("cursor"));
        write_yaml(&path, &Value::Mapping(first)).unwrap();
        assert_eq!(
            read_yaml::<Value>(&path)
                .unwrap()
                .as_mapping()
                .unwrap()
                .get(s("editor_command")),
            Some(&s("cursor"))
        );

        // A second write fully replaces the first, and the atomic-rename tmp is
        // gone (cleaned up by the successful rename, not orphaned beside it).
        let mut second = serde_norway::Mapping::new();
        second.insert(s("terminal_app"), s("iTerm"));
        write_yaml(&path, &Value::Mapping(second)).unwrap();
        let reread = read_yaml::<Value>(&path).unwrap();
        let map = reread.as_mapping().unwrap();
        assert_eq!(map.get(s("terminal_app")), Some(&s("iTerm")));
        assert!(
            !map.contains_key(s("editor_command")),
            "second write replaces the first"
        );
        assert!(
            !tmp_path(&path).exists(),
            "the temp file must not be left behind"
        );
    }

    #[test]
    fn edit_seeds_a_missing_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.yaml");

        edit_yaml_mapping(&path, |root| {
            root.insert(s("editor_command"), s("cursor"));
        })
        .unwrap();

        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains("editor_command") && raw.contains("cursor"));
    }

    #[test]
    fn edit_preserves_keys_the_closure_does_not_touch() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        std::fs::write(
            &path,
            "mcp_providers:\n  - name: team\nterminal_app: iTerm\n",
        )
        .unwrap();

        edit_yaml_mapping(&path, |root| {
            root.insert(s("editor_command"), s("zed"));
        })
        .unwrap();

        let reread: Value = read_yaml(&path).unwrap();
        let map = reread.as_mapping().unwrap();
        assert!(
            map.contains_key(s("mcp_providers")),
            "untouched key survives"
        );
        assert_eq!(map.get(s("terminal_app")), Some(&s("iTerm")));
        assert_eq!(map.get(s("editor_command")), Some(&s("zed")));
    }

    #[test]
    fn edit_refuses_a_non_mapping_and_leaves_it_untouched() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        std::fs::write(&path, "just a scalar, not a mapping\n").unwrap();

        let result = edit_yaml_mapping(&path, |root| {
            root.insert(s("editor_command"), s("cursor"));
        });
        assert!(matches!(result, Err(CoreError::NotAMapping { .. })));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "just a scalar, not a mapping\n",
            "the original file must not be overwritten"
        );
    }

    #[test]
    fn empty_and_null_files_are_treated_as_empty_mappings() {
        let dir = tempdir().unwrap();
        for (name, contents) in [("blank.yaml", "   \n"), ("null.yaml", "null\n")] {
            let path = dir.path().join(name);
            std::fs::write(&path, contents).unwrap();
            edit_yaml_mapping(&path, |root| {
                root.insert(s("k"), s("v"));
            })
            .unwrap();
            let reread: Value = read_yaml(&path).unwrap();
            assert_eq!(reread.as_mapping().unwrap().get(s("k")), Some(&s("v")));
        }
    }

    #[test]
    fn concurrent_edits_from_two_subsystems_both_survive() {
        // The whole reason this helper exists: two independent writers doing
        // read-modify-write on the same shared config must not drop each other's
        // keys. Hammer it from many threads — each adds its own key — and assert
        // every key landed.
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        std::fs::write(&path, "{}\n").unwrap();

        let threads: Vec<_> = (0..16)
            .map(|i| {
                let path = path.clone();
                std::thread::spawn(move || {
                    edit_yaml_mapping(&path, |root| {
                        root.insert(s(&format!("key{i}")), Value::from(i));
                    })
                    .unwrap();
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }

        let reread: Value = read_yaml(&path).unwrap();
        let map = reread.as_mapping().unwrap();
        for i in 0..16 {
            assert_eq!(
                map.get(s(&format!("key{i}"))),
                Some(&Value::from(i)),
                "key{i} must survive concurrent writes"
            );
        }
    }
}
