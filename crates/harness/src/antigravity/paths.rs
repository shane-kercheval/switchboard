//! Filesystem paths Antigravity creates and Switchboard reads.
//!
//! Antigravity stores per-conversation state under
//! `~/.gemini/antigravity-cli/` (yes — under `.gemini/`; the CLI binary
//! explicitly resolves `GeminiDir` and falls back to that path). The only
//! plaintext-parseable record of a conversation is the JSONL transcript
//! under `brain/<uuid>/.system_generated/logs/transcript.jsonl`.
//!
//! **The `.system_generated/` segment is a Google-internal artifact.** The
//! research probe found this name in the binary's `strings` output with no
//! public documentation; Google has not committed to keeping it stable
//! between Antigravity versions. If a future `agy` release moves the
//! transcript, every downstream consumer (live tail, hydration, attach)
//! breaks at the same point. Keeping the path as a single function here
//! lets a future maintainer fix the breakage by editing one line. Do not
//! inline these strings at call sites.

use std::path::{Path, PathBuf};

use uuid::Uuid;

/// The `~/.gemini/antigravity-cli/` directory where Antigravity stores all
/// per-user state. Named `GeminiDir` inside the `agy` binary; the shared
/// `~/.gemini/` namespace is load-bearing — `agy` reads
/// `~/.gemini/oauth_creds.json`, `settings.json`, and `installation_id`
/// from the parent directory (Gemini-CLI residue) but writes only to its
/// own `antigravity-cli/` subtree.
#[must_use]
pub fn antigravity_root(home_dir: &Path) -> PathBuf {
    home_dir.join(".gemini").join("antigravity-cli")
}

/// The `~/.gemini/config/` directory holding Antigravity's MCP-server config
/// and plugin/skill tree. Note this is `~/.gemini/config/`, **not**
/// `~/.gemini/antigravity-cli/` — the registries live in the shared
/// `~/.gemini/` namespace alongside (not under) the per-conversation state.
#[must_use]
pub fn config_root(home_dir: &Path) -> PathBuf {
    home_dir.join(".gemini").join("config")
}

/// The MCP-server config file: `~/.gemini/config/mcp_config.json`. Top-level
/// `{ "mcpServers": { … } }` — same schema as Gemini's `settings.json`
/// `mcpServers` key, different file.
#[must_use]
pub fn mcp_config_path(home_dir: &Path) -> PathBuf {
    config_root(home_dir).join("mcp_config.json")
}

/// The plugins root: `~/.gemini/config/plugins/`. Each `<plugin>/skills/<skill>/`
/// subtree with a `SKILL.md` is one skill, displayed qualified as
/// `<plugin>/<skill>`.
#[must_use]
pub fn plugins_root(home_dir: &Path) -> PathBuf {
    config_root(home_dir).join("plugins")
}

/// The `brain/` directory holding one subdirectory per conversation. The
/// adapter watches this for a newly-created `<uuid>/` directory to capture
/// the server-assigned conversation UUID after a first-turn spawn.
#[must_use]
pub fn brain_root(home_dir: &Path) -> PathBuf {
    antigravity_root(home_dir).join("brain")
}

/// The per-conversation directory under `brain/`. Created by `agy` when a
/// new conversation begins; its appearance is the adapter's signal that
/// the server-assigned UUID has been captured.
#[must_use]
pub fn conversation_brain_dir(home_dir: &Path, conversation_id: Uuid) -> PathBuf {
    brain_root(home_dir).join(conversation_id.to_string())
}

/// The plaintext JSONL transcript for a conversation. **This is the only
/// parseable record** of the conversation — the canonical `.pb` file in
/// `conversations/<uuid>.pb` is encrypted protobuf.
///
/// Path: `<home>/.gemini/antigravity-cli/brain/<uuid>/.system_generated/logs/transcript.jsonl`.
///
/// See the module-level docstring for why `.system_generated/` is the most
/// brittle segment of this path.
#[must_use]
pub fn transcript_path(home_dir: &Path, conversation_id: Uuid) -> PathBuf {
    conversation_brain_dir(home_dir, conversation_id)
        .join(".system_generated")
        .join("logs")
        .join("transcript.jsonl")
}

/// The richer transcript written by current Antigravity versions. It carries
/// the same record sequence as `transcript.jsonl`, but tool arguments retain
/// their native JSON types and are not clipped by the compact log formatter.
#[must_use]
pub(crate) fn full_transcript_path(home_dir: &Path, conversation_id: Uuid) -> PathBuf {
    conversation_brain_dir(home_dir, conversation_id)
        .join(".system_generated")
        .join("logs")
        .join("transcript_full.jsonl")
}

/// Prefer the lossless transcript only after it is at least as complete as the
/// compact representation. Antigravity writes the files independently, so
/// existence alone is not proof that the full file has caught up.
#[must_use]
pub(crate) fn preferred_transcript_path(home_dir: &Path, conversation_id: Uuid) -> PathBuf {
    let compact = transcript_path(home_dir, conversation_id);
    let full = full_transcript_path(home_dir, conversation_id);
    let full_lines = complete_line_count(&full);
    if full_lines > 0 && full_lines >= complete_line_count(&compact) {
        full
    } else {
        compact
    }
}

/// Return the full transcript only when it can safely inherit a cursor that
/// has already emitted `cursor` compact records.
#[must_use]
pub(crate) fn caught_up_full_transcript_path(
    home_dir: &Path,
    conversation_id: Uuid,
    cursor: usize,
) -> Option<PathBuf> {
    let full = full_transcript_path(home_dir, conversation_id);
    let full_lines = complete_line_count(&full);
    (full_lines > 0 && full_lines >= cursor).then_some(full)
}

#[must_use]
pub(crate) fn complete_line_count(path: &Path) -> usize {
    std::fs::read_to_string(path).map_or(0, |content| match content.rfind('\n') {
        Some(idx) => content[..=idx].lines().count(),
        None => 0,
    })
}

#[must_use]
pub(crate) fn is_full_transcript_path(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|name| name == "transcript_full.jsonl")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transcript_path_matches_observed_layout() {
        let home = Path::new("/Users/test");
        let uuid = Uuid::parse_str("01234567-89ab-cdef-0123-456789abcdef").unwrap();
        let path = transcript_path(home, uuid);
        assert_eq!(
            path,
            PathBuf::from(
                "/Users/test/.gemini/antigravity-cli/brain/01234567-89ab-cdef-0123-456789abcdef/.system_generated/logs/transcript.jsonl"
            )
        );
    }

    #[test]
    fn preferred_transcript_uses_full_only_after_it_catches_compact() {
        let home = tempfile::tempdir().unwrap();
        let uuid = Uuid::now_v7();
        let compact = transcript_path(home.path(), uuid);
        let full = full_transcript_path(home.path(), uuid);
        std::fs::create_dir_all(compact.parent().unwrap()).unwrap();
        assert_eq!(preferred_transcript_path(home.path(), uuid), compact);

        std::fs::write(&compact, "compact one\ncompact two\n").unwrap();
        std::fs::write(&full, "full one\n").unwrap();
        assert_eq!(preferred_transcript_path(home.path(), uuid), compact);

        std::fs::write(&full, "full one\nfull two\n").unwrap();
        assert_eq!(preferred_transcript_path(home.path(), uuid), full);
    }

    #[test]
    fn conversation_brain_dir_is_prefix_of_transcript_path() {
        // The transcript path must always live under the conversation's
        // brain directory; the directory's appearance is the adapter's
        // signal that the UUID was captured, and the transcript is then
        // expected somewhere below it.
        let home = Path::new("/h");
        let uuid = Uuid::new_v4();
        let dir = conversation_brain_dir(home, uuid);
        let transcript = transcript_path(home, uuid);
        assert!(transcript.starts_with(&dir));
    }
}
