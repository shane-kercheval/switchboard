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
pub(crate) fn append_jsonl<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let line = serde_json::to_string(value).map_err(|source| CoreError::Serialize {
        path: path.to_owned(),
        source,
    })?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| CoreError::io(path, e))?;
    writeln!(file, "{line}").map_err(|e| CoreError::io(path, e))?;
    file.flush().map_err(|e| CoreError::io(path, e))?;
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

pub(crate) fn read_yaml<T: DeserializeOwned>(path: &Path) -> Result<T> {
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
pub(crate) fn write_yaml<T: Serialize>(path: &Path, value: &T) -> Result<()> {
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
