//! Gemini session-file path helpers.
//!
//! Gemini stores session files under
//! `~/.gemini/tmp/<project-name>/chats/session-<YYYY-MM-DDTHH-MM>-<id8>.jsonl`.
//! `<project-name>` is recorded in `~/.gemini/projects.json` (cwd → name
//! mapping, populated on first headless dispatch). `<id8>` is the first 8
//! hex characters of the session UUID — only 32 bits of disambiguation, so
//! the helpers exposed here return a *candidate set*; callers verify by
//! reading the header `sessionId` field when collision-safety is required.
//!
//! **What this module owns in M3.2.**
//! - Cwd → project-name lookup (`resolve_gemini_project_name`).
//! - First-8-char-prefix glob over the chats directory
//!   (`gemini_session_file_candidates`).
//! - A single-file existence check used by `build_args` to pick
//!   `--session-id` vs `--resume` (`session_file_exists_for`).
//!
//! Transcript hydration (`load_gemini_transcript`) lands in M3.3 on top of
//! these helpers.

use std::path::{Path, PathBuf};

use uuid::Uuid;

/// Take the first 8 hex chars of a UUID as Gemini's filename suffix would.
/// Lowercase (Gemini emits lowercase hex in filenames).
#[must_use]
pub fn id_prefix(session_id: &Uuid) -> String {
    // `simple()` is 32 hex chars, no dashes. Slice the first 8.
    let simple = session_id.simple().to_string();
    simple[..8].to_owned()
}

/// Resolve cwd → Gemini project-name via `~/.gemini/projects.json`.
/// Returns `None` if the file is missing, unreadable, the cwd doesn't
/// exist on disk, or `projects.json` contains no entry for the canonical
/// cwd. Degrading to `None` lets the caller treat "never-dispatched-yet"
/// identically to "lookup failed" — both produce an empty candidate set.
///
/// **Cwd is canonicalized internally** (matches Claude's `session_exists_in`
/// pattern). Gemini's `projects.json` is keyed by its own resolved working
/// directory — on macOS, `/tmp/X` becomes `/private/tmp/X`. Without
/// canonicalization, a non-canonical caller cwd would miss an existing
/// entry, `build_args` would pick `--session-id` on the second turn, and
/// Gemini would exit 42 because the session already exists. The
/// production caller (`Directory::at`) canonicalizes already; this is
/// defensive parity.
#[must_use]
pub fn resolve_gemini_project_name(home_dir: &Path, cwd: &Path) -> Option<String> {
    let canonical = cwd.canonicalize().ok()?;
    let path = home_dir.join(".gemini").join("projects.json");
    let bytes = std::fs::read(&path).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let cwd_str = canonical.to_str()?;
    // The file shape observed in M3.1 is `{"projects": {"<abs-cwd>": "<name>"}}`.
    // The "projects" wrapper key isn't guaranteed across Gemini CLI versions —
    // try both shapes (wrapped + flat) so we degrade gracefully.
    let map = value
        .get("projects")
        .and_then(serde_json::Value::as_object)
        .or_else(|| value.as_object())?;
    map.get(cwd_str)
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

/// Path to Gemini's chats directory for `<project-name>`. Does not check
/// existence; callers glob inside it.
fn chats_dir(home_dir: &Path, project_name: &str) -> PathBuf {
    home_dir
        .join(".gemini")
        .join("tmp")
        .join(project_name)
        .join("chats")
}

/// Enumerate session-file candidates matching `session-*-<id8>.jsonl` in
/// the project's chats directory. Returns an empty vector if the cwd
/// doesn't exist, the project mapping is unknown, or the chats directory
/// is missing or unreadable. Cwd is canonicalized internally via
/// `resolve_gemini_project_name`. Used by:
/// - `session_file_exists_for` (pick `--session-id` vs `--resume`).
/// - The attach lookup (pick the right file when filenames collide; M3.4).
/// - The transcript hydrator (M3.3).
#[must_use]
pub fn gemini_session_file_candidates(
    home_dir: &Path,
    cwd: &Path,
    session_id: &Uuid,
) -> Vec<PathBuf> {
    let Some(project_name) = resolve_gemini_project_name(home_dir, cwd) else {
        return Vec::new();
    };
    let dir = chats_dir(home_dir, &project_name);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let suffix = format!("-{}.jsonl", id_prefix(session_id));
    let prefix = "session-";
    let mut hits = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if name.starts_with(prefix) && name.ends_with(&suffix) {
            hits.push(path);
        }
    }
    hits
}

/// True if at least one session file matches the prefix for `session_id` in
/// the project's chats directory. Used by `build_args` to decide between
/// `--session-id` (first turn) and `--resume` (subsequent turns), mirroring
/// the Claude Code pattern.
///
/// **Why prefix-only is correct here**: under Switchboard's UUID-v4 policy
/// for Gemini session IDs, the first-8-char collision probability is
/// ~1/2^32. Existence-by-prefix is effectively existence-by-full-id. A
/// future cross-collision under an external test fixture would mis-route a
/// first turn as a resume — handled by `--resume <unknown-uuid>` failing
/// with exit 42, surfaced as `AdapterFailure`. Acceptable v1 behavior.
#[must_use]
pub fn session_file_exists_for(home_dir: &Path, cwd: &Path, session_id: &Uuid) -> bool {
    !gemini_session_file_candidates(home_dir, cwd, session_id).is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn id_prefix_takes_first_8_hex_chars_lowercase() {
        let uuid = Uuid::parse_str("ABCDEF12-3456-4789-89AB-CDEF01234567").unwrap();
        assert_eq!(id_prefix(&uuid), "abcdef12");
    }

    /// Write `projects.json` mapping the canonical form of `cwd` → name.
    /// Matches what the real Gemini CLI writes (it canonicalizes cwd before
    /// recording the key).
    fn stage_projects_json_wrapped(home: &Path, cwd: &Path, name: &str) {
        let gemini = home.join(".gemini");
        std::fs::create_dir_all(&gemini).unwrap();
        let canonical = cwd.canonicalize().unwrap();
        let body = serde_json::json!({
            "projects": { canonical.to_str().unwrap(): name }
        });
        std::fs::write(gemini.join("projects.json"), body.to_string()).unwrap();
    }

    fn stage_projects_json_flat(home: &Path, cwd: &Path, name: &str) {
        let gemini = home.join(".gemini");
        std::fs::create_dir_all(&gemini).unwrap();
        let canonical = cwd.canonicalize().unwrap();
        let body = serde_json::json!({
            canonical.to_str().unwrap(): name
        });
        std::fs::write(gemini.join("projects.json"), body.to_string()).unwrap();
    }

    #[test]
    fn resolve_returns_none_when_projects_json_missing() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        assert!(resolve_gemini_project_name(home.path(), cwd.path()).is_none());
    }

    #[test]
    fn resolve_returns_none_when_cwd_does_not_exist() {
        // Canonicalize-fail → None. Non-existent cwd is indistinguishable
        // from "never dispatched" at the lookup boundary.
        let home = TempDir::new().unwrap();
        assert!(
            resolve_gemini_project_name(home.path(), Path::new("/definitely/not/a/real/path"))
                .is_none()
        );
    }

    #[test]
    fn resolve_reads_wrapped_projects_map() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        stage_projects_json_wrapped(home.path(), cwd.path(), "my-project");
        assert_eq!(
            resolve_gemini_project_name(home.path(), cwd.path()),
            Some("my-project".to_owned())
        );
    }

    #[test]
    fn resolve_reads_flat_projects_map() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        stage_projects_json_flat(home.path(), cwd.path(), "flat-name");
        assert_eq!(
            resolve_gemini_project_name(home.path(), cwd.path()),
            Some("flat-name".to_owned())
        );
    }

    /// Regression: cwd canonicalization must happen inside the lookup
    /// helper so non-canonical caller paths (symlinks, `/tmp` vs
    /// `/private/tmp` on macOS) still resolve to the canonical key the
    /// Gemini CLI wrote. Without this, second-turn dispatches would
    /// fall through to `--session-id`, Gemini exits 42, agents fail
    /// silently after their first turn.
    #[cfg(unix)]
    #[test]
    fn resolve_canonicalizes_cwd_against_symlinked_path() {
        let home = TempDir::new().unwrap();
        let real = TempDir::new().unwrap();
        let link_parent = TempDir::new().unwrap();
        let link = link_parent.path().join("symlink-to-cwd");
        std::os::unix::fs::symlink(real.path(), &link).unwrap();
        stage_projects_json_wrapped(home.path(), real.path(), "via-canonical");

        assert_eq!(
            resolve_gemini_project_name(home.path(), &link),
            Some("via-canonical".to_owned()),
            "non-canonical cwd must resolve via canonicalize() to the key Gemini wrote"
        );
    }

    #[test]
    fn candidates_empty_when_project_unknown() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let uuid = Uuid::new_v4();
        let hits = gemini_session_file_candidates(home.path(), cwd.path(), &uuid);
        assert!(hits.is_empty());
    }

    #[test]
    fn candidates_match_only_files_with_session_prefix_and_id_suffix() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        stage_projects_json_wrapped(home.path(), cwd.path(), "proj");
        let chats = home
            .path()
            .join(".gemini")
            .join("tmp")
            .join("proj")
            .join("chats");
        std::fs::create_dir_all(&chats).unwrap();

        let uuid = Uuid::parse_str("00000000-0000-4000-8000-000000000001").unwrap();
        let prefix = id_prefix(&uuid);
        let matching = chats.join(format!("session-2026-05-17T22-11-{prefix}.jsonl"));
        let non_matching_suffix = chats.join("session-2026-05-17T22-11-deadbeef.jsonl");
        let non_matching_prefix = chats.join(format!("rollout-other-{prefix}.jsonl"));
        std::fs::write(&matching, "").unwrap();
        std::fs::write(&non_matching_suffix, "").unwrap();
        std::fs::write(&non_matching_prefix, "").unwrap();

        let hits = gemini_session_file_candidates(home.path(), cwd.path(), &uuid);
        assert_eq!(hits, vec![matching]);
    }

    #[test]
    fn session_file_exists_for_picks_up_matching_file() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        stage_projects_json_wrapped(home.path(), cwd.path(), "proj");
        let chats = home
            .path()
            .join(".gemini")
            .join("tmp")
            .join("proj")
            .join("chats");
        std::fs::create_dir_all(&chats).unwrap();

        let uuid = Uuid::parse_str("00000000-0000-4000-8000-000000000001").unwrap();
        let prefix = id_prefix(&uuid);
        std::fs::write(
            chats.join(format!("session-2026-05-17T22-11-{prefix}.jsonl")),
            "",
        )
        .unwrap();

        assert!(session_file_exists_for(home.path(), cwd.path(), &uuid));
        let other = Uuid::new_v4();
        assert!(!session_file_exists_for(home.path(), cwd.path(), &other));
    }
}
