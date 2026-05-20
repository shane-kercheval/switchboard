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

/// The per-conversation directory under `brain/`. Created by `agy` when a
/// new conversation begins; its appearance is the adapter's signal that
/// the server-assigned UUID has been captured.
#[must_use]
pub fn conversation_brain_dir(home_dir: &Path, conversation_id: Uuid) -> PathBuf {
    antigravity_root(home_dir)
        .join("brain")
        .join(conversation_id.to_string())
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
