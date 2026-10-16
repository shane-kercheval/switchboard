//! Codex rollout files, plain or compressed.
//!
//! Codex compresses a rollout nobody has written to for a week: it writes
//! `rollout-*.jsonl.zst`, a zstd copy of the same bytes, then deletes the plain
//! file. Before appending to one, it turns it back into plain `.jsonl`
//! (`codex-rs/rollout/src/compression.rs` at `rust-v0.162.1`;
//! `docs/harness-behavior.md` G40). Either way it writes the new copy before
//! deleting the old one, and while both exist it reads the plain one. Every
//! lookup here follows the same rule, so Switchboard reads the file Codex
//! would.
//!
//! **A conversion can land between finding a file and opening it.** So a
//! lookup resolves each file it found to the copy that exists now, and the
//! opener, finding its copy gone, opens the other one. Because Codex writes the
//! new copy before deleting the old, the other copy is already there; one
//! switch is enough, with no waiting.

use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

const COMPRESSED_SUFFIX: &str = ".zst";

/// Whether `name` is the rollout file of `session_id`, plain or compressed.
/// Codex's temporary files, which add a `.tmp` suffix, are not.
pub(crate) fn is_rollout_of(name: &str, session_id: &str) -> bool {
    let plain = name.strip_suffix(COMPRESSED_SUFFIX).unwrap_or(name);
    plain.starts_with("rollout-") && plain.ends_with(&format!("-{session_id}.jsonl"))
}

/// The rollout name without the compression suffix, when `name` has it.
pub(crate) fn plain_name(name: &str) -> &str {
    if is_compressed(name) {
        name.strip_suffix(COMPRESSED_SUFFIX).unwrap_or(name)
    } else {
        name
    }
}

/// Whether `path` is a compressed rollout (`.jsonl.zst`).
pub(crate) fn is_compressed_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(is_compressed)
}

/// The copy of this rollout Codex would read now: the plain file when it
/// exists, as Codex prefers it (its `should_skip_compressed_sibling`). A
/// directory listing that saw only the compressed copy may find the plain one
/// has since appeared, because Codex decompresses a file before appending to
/// it; the plain copy is then the one to use, not neither.
pub(crate) fn current_copy(path: PathBuf) -> PathBuf {
    if is_compressed_path(&path)
        && let Some(plain) = other_copy(&path)
        && plain.exists()
    {
        return plain;
    }
    path
}

/// Each found rollout as [`current_copy`], once. A plain file and its
/// compressed copy are one rollout.
pub(crate) fn current_copies(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut copies: Vec<PathBuf> = Vec::new();
    for path in paths.into_iter().map(current_copy) {
        if !copies.contains(&path) {
            copies.push(path);
        }
    }
    copies
}

/// Open a rollout for reading, decompressing a `.jsonl.zst` one as it reads.
///
/// **May read the other copy.** If `path` is gone because Codex converted it
/// after the lookup, the plain or compressed copy that replaced it is opened
/// instead. Only when neither exists does this fail with `NotFound`.
///
/// A compressed file that doesn't decode, or that ends partway through, fails
/// the read rather than ending early, so a damaged file can't pass for a
/// shorter conversation.
pub(crate) fn open(path: &Path) -> io::Result<Box<dyn Read>> {
    match open_exactly(path) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            let Some(other) = other_copy(path) else {
                return Err(err);
            };
            open_exactly(&other).map_err(|other_err| {
                if other_err.kind() == io::ErrorKind::NotFound {
                    err
                } else {
                    other_err
                }
            })
        }
        opened => opened,
    }
}

fn open_exactly(path: &Path) -> io::Result<Box<dyn Read>> {
    let file = File::open(path)?;
    if is_compressed_path(path) {
        Ok(Box::new(zstd::stream::read::Decoder::new(file)?))
    } else {
        Ok(Box::new(file))
    }
}

/// The same rollout in its other form: `.jsonl` for `.jsonl.zst` and the
/// reverse. `None` for a name that is neither.
fn other_copy(path: &Path) -> Option<PathBuf> {
    let name = path.file_name()?.to_str()?;
    if is_compressed(name) {
        Some(path.with_file_name(plain_name(name)))
    } else if path
        .extension()
        .is_some_and(|extension| extension == "jsonl")
    {
        Some(path.with_file_name(format!("{name}{COMPRESSED_SUFFIX}")))
    } else {
        None
    }
}

/// Read a whole rollout as text, decompressing a `.jsonl.zst` one.
pub(crate) fn read_to_string(path: &Path) -> io::Result<String> {
    let mut content = String::new();
    open(path)?.read_to_string(&mut content)?;
    Ok(content)
}

fn is_compressed(name: &str) -> bool {
    name.ends_with(".jsonl.zst")
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "01a10276-e1a3-7cc3-93e7-57102fc059f4";

    #[test]
    fn both_forms_of_a_sessions_rollout_name_match_and_nothing_else_does() {
        assert!(is_rollout_of(
            &format!("rollout-2026-10-03T10-51-47-{ID}.jsonl"),
            ID
        ));
        assert!(is_rollout_of(
            &format!("rollout-2026-10-03T10-51-47-{ID}.jsonl.zst"),
            ID
        ));
        for name in [
            format!("rollout-2026-10-03T10-51-47-{ID}.jsonl.zst.compress.4242.0.tmp"),
            format!("rollout-2026-10-03T10-51-47-{ID}.jsonl.decompress.4242.0.tmp"),
            format!("rollout-2026-10-03T10-51-47-{ID}.jsonl.zst.zst"),
            format!("rollout-2026-10-03T10-51-47-{ID}.json"),
            format!("other-{ID}.jsonl.zst"),
            "rollout-2026-10-03T10-51-47-another-id.jsonl.zst".to_owned(),
        ] {
            assert!(!is_rollout_of(&name, ID), "{name}");
        }
    }

    fn compressed(content: &str) -> Vec<u8> {
        zstd::encode_all(content.as_bytes(), 3).unwrap()
    }

    #[test]
    fn opening_a_plain_rollout_that_codex_just_compressed_reads_the_compressed_copy() {
        let dir = tempfile::TempDir::new().unwrap();
        let plain = dir.path().join(format!("rollout-x-{ID}.jsonl"));
        std::fs::write(
            dir.path().join(format!("rollout-x-{ID}.jsonl.zst")),
            compressed("a\n"),
        )
        .unwrap();

        assert_eq!(read_to_string(&plain).unwrap(), "a\n");
    }

    #[test]
    fn opening_a_compressed_rollout_that_codex_just_decompressed_reads_the_plain_copy() {
        let dir = tempfile::TempDir::new().unwrap();
        let zst = dir.path().join(format!("rollout-x-{ID}.jsonl.zst"));
        std::fs::write(dir.path().join(format!("rollout-x-{ID}.jsonl")), "a\n").unwrap();

        assert_eq!(read_to_string(&zst).unwrap(), "a\n");
    }

    #[test]
    fn opening_a_rollout_with_neither_copy_fails_as_not_found() {
        let dir = tempfile::TempDir::new().unwrap();
        for name in [
            format!("rollout-x-{ID}.jsonl"),
            format!("rollout-x-{ID}.jsonl.zst"),
        ] {
            let error = read_to_string(&dir.path().join(&name)).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::NotFound, "{name}");
        }
    }

    #[test]
    fn a_found_compressed_copy_resolves_to_a_plain_copy_that_has_since_appeared() {
        // The listing saw only the compressed copy; Codex then decompressed it
        // to append. Dropping the compressed entry because the plain one now
        // exists would leave nothing.
        let dir = tempfile::TempDir::new().unwrap();
        let plain = dir.path().join(format!("rollout-x-{ID}.jsonl"));
        let zst = dir.path().join(format!("rollout-x-{ID}.jsonl.zst"));
        std::fs::write(&plain, "a\n").unwrap();
        std::fs::write(&zst, compressed("a\n")).unwrap();

        assert_eq!(current_copies(vec![zst.clone()]), vec![plain.clone()]);
        assert_eq!(current_copies(vec![zst, plain.clone()]), vec![plain]);
    }

    #[test]
    fn a_compressed_copy_with_no_plain_file_stays_the_current_copy() {
        let dir = tempfile::TempDir::new().unwrap();
        let zst = dir.path().join(format!("rollout-x-{ID}.jsonl.zst"));
        std::fs::write(&zst, compressed("a\n")).unwrap();

        assert_eq!(current_copies(vec![zst.clone()]), [zst]);
    }

    #[test]
    fn the_plain_name_drops_only_the_compression_suffix() {
        assert_eq!(plain_name("rollout-x.jsonl.zst"), "rollout-x.jsonl");
        assert_eq!(plain_name("rollout-x.jsonl"), "rollout-x.jsonl");
        assert_eq!(plain_name("rollout-x.zst"), "rollout-x.zst");
    }
}
