//! Attachment metadata — files a user attaches to a send for the agent(s) to
//! read.
//!
//! An attachment's bytes live on disk under the project's `attachments/` dir;
//! [`Attachment`] is the structured record that travels on the journal `Send`
//! and across the IPC wire. It is kept deliberately separate from the prompt
//! text: the UI renders chips/thumbnails from the structured list, while the
//! agent receives a path-in-text footer built by [`render_prompt_with_attachments`].

use serde::{Deserialize, Serialize};

/// How a staged attachment is classified, by the dropped file's extension.
/// Drives the user-facing label prefix (`image-1`, `text-1`, `file-1`).
///
/// The classification is assigned **frontend-side** (it owns the extension→kind
/// mapping and has the dropped filename); core only persists what it's told.
///
/// `#[non_exhaustive]` keeps adding a variant non-breaking for Rust `match`
/// callers, but that alone does **not** make older builds tolerate a newer kind
/// read from the persisted journal — serde would reject the unknown string and
/// fail the whole journal load. So `Unknown` (`#[serde(other)]`) is the
/// deserialize fallback for an unrecognized kind: a *display-only* hint must
/// never brick a project's history. This is a deliberate exception to the
/// journal's fail-loud invariant — structural [`crate::JournalRecord`] variants
/// still fail loud (we can't render a record we don't understand), but a
/// cosmetic classification degrades to a generic file. `Unknown` is never
/// constructed by our code (the frontend only ever emits the three real kinds)
/// and never serialized, so it costs no round-trip fidelity on an append-only
/// journal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AttachmentKind {
    Image,
    Text,
    File,
    #[serde(other)]
    Unknown,
}

/// Metadata for one staged file attached to a send.
///
/// The user message is rendered from this structured list (clean prompt text +
/// chips), never from the agent-facing footer — so the UI never shows raw paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    /// User-facing reference, e.g. `image-1`. Assigned by the frontend; the
    /// inline `@`-menu token and the agent-facing footer use this same label.
    pub label: String,
    pub kind: AttachmentKind,
    /// Absolute path to the staged file under the store's `attachments/` dir.
    /// **The live location** — where the file is now, so this is what rendering
    /// and the reference-GC use.
    pub path: String,
    /// The path this attachment was **dispatched under**, when that differs from
    /// where the file lives today. `None` (the common case, and what records
    /// written before this field existed deserialize to) means "same as `path`".
    ///
    /// It exists because a journal entry has two jobs that pull apart the moment
    /// a file moves. Reading it back to *find the file* wants the current path;
    /// reconstructing *what was sent* wants the original. The surplus-case
    /// send↔turn correlation (`align_surplus_candidates` in `crates/app/src/commands.rs`)
    /// re-renders the exact prompt text the harness recorded and matches it
    /// character-for-character — so if a migration rewrote `path`, every
    /// attachment-bearing send before the move would stop reconstructing and its
    /// reply would render as an unattributed duplicate.
    ///
    /// Read it through [`Self::sent_path`], never directly, so the fallback
    /// lives in one place. **Migration is the only writer of a divergent
    /// value**: nothing in normal operation moves a staged file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dispatched_path: Option<String>,
    /// The dropped file's basename, for display.
    pub original_name: String,
}

impl Attachment {
    /// The path this attachment was dispatched under — `dispatched_path` when a
    /// migration recorded one, else the live `path`.
    ///
    /// **Only prompt reconstruction wants this.** Anything that needs to *touch*
    /// the file (rendering a thumbnail, staging, the reference-GC's liveness
    /// check) wants `path`, because that is where the bytes are.
    #[must_use]
    pub fn sent_path(&self) -> &str {
        self.dispatched_path.as_deref().unwrap_or(&self.path)
    }
}

/// Build the agent-facing prompt: the clean prompt followed by a footer listing
/// each attachment as `label: <absolute path>`.
///
/// **Why path-in-text, not a native multimodal/image channel.** Every harness
/// adapter passes the prompt to its CLI as a plain string argument with no image
/// input channel — but all of them have file-reading tools. So the
/// harness-agnostic transport is to stage the file on disk and hand the agent
/// its absolute path to read; this is the correct approach for this
/// architecture, not a workaround, and should not be "upgraded" to a per-harness
/// image API. The path is absolute and staged inside the project's working dir
/// so it resolves under every harness's sandbox.
///
/// The footer exists **only** in the string handed to the adapter. The journal
/// and UI keep the clean prompt plus the structured [`Attachment`] list
/// separately, so display stays clean while the agent still gets the paths.
///
/// Returns the prompt unchanged when there are no attachments (no trailing
/// separator).
#[must_use]
pub fn render_prompt_with_attachments(prompt: &str, attachments: &[Attachment]) -> String {
    render_footer(prompt, attachments, |a| &a.path)
}

/// Reconstruct the prompt text a send was **dispatched with**, for matching
/// against what the harness recorded.
///
/// Identical to [`render_prompt_with_attachments`] except that it reads each
/// attachment's [`Attachment::dispatched_path`]. Separate functions rather than
/// a bool parameter: the two have different *callers* rather than different
/// modes — one builds text to hand a live agent, the other rebuilds text that
/// was already sent — and a bool at a call site reads as neither.
#[must_use]
pub fn render_dispatched_prompt_with_attachments(
    prompt: &str,
    attachments: &[Attachment],
) -> String {
    render_footer(prompt, attachments, Attachment::sent_path)
}

fn render_footer<'a>(
    prompt: &str,
    attachments: &'a [Attachment],
    path_of: impl Fn(&'a Attachment) -> &'a str,
) -> String {
    if attachments.is_empty() {
        return prompt.to_owned();
    }
    let mut out = String::from(prompt);
    out.push_str("\n\n---\nAttached files (read them):");
    for attachment in attachments {
        out.push('\n');
        out.push_str(&attachment.label);
        out.push_str(": ");
        out.push_str(path_of(attachment));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attachment(label: &str, kind: AttachmentKind, path: &str, name: &str) -> Attachment {
        Attachment {
            label: label.to_owned(),
            kind,
            path: path.to_owned(),
            dispatched_path: None,
            original_name: name.to_owned(),
        }
    }

    #[test]
    fn attachment_round_trips_through_json() {
        let a = attachment("image-1", AttachmentKind::Image, "/p/img.png", "img.png");
        let json = serde_json::to_string(&a).unwrap();
        assert_eq!(serde_json::from_str::<Attachment>(&json).unwrap(), a);
    }

    #[test]
    fn kind_serializes_snake_case() {
        assert_eq!(
            serde_json::to_value(AttachmentKind::Image).unwrap(),
            serde_json::json!("image")
        );
        assert_eq!(
            serde_json::to_value(AttachmentKind::Text).unwrap(),
            serde_json::json!("text")
        );
        assert_eq!(
            serde_json::to_value(AttachmentKind::File).unwrap(),
            serde_json::json!("file")
        );
    }

    #[test]
    fn unknown_kind_degrades_instead_of_failing() {
        // A kind string written by a newer build (e.g. "audio") must not fail
        // deserialization — a display-only hint can't be allowed to brick a
        // project's history. It degrades to `Unknown`; the other fields survive.
        let a: Attachment = serde_json::from_value(serde_json::json!({
            "label": "audio-1",
            "kind": "audio",
            "path": "/a/clip.mp3",
            "original_name": "clip.mp3",
        }))
        .unwrap();
        assert_eq!(a.kind, AttachmentKind::Unknown);
        assert_eq!(a.original_name, "clip.mp3");
    }

    #[test]
    fn empty_attachments_leave_prompt_untouched() {
        assert_eq!(render_prompt_with_attachments("hello", &[]), "hello");
    }

    #[test]
    fn one_attachment_appends_footer_in_exact_shape() {
        let rendered = render_prompt_with_attachments(
            "compare these",
            &[attachment(
                "image-1",
                AttachmentKind::Image,
                "/proj/.switchboard/projects/x/attachments/u__diagram.png",
                "diagram.png",
            )],
        );
        assert_eq!(
            rendered,
            "compare these\n\n---\nAttached files (read them):\n\
             image-1: /proj/.switchboard/projects/x/attachments/u__diagram.png"
        );
    }

    #[test]
    fn many_attachments_each_get_a_label_path_line() {
        let rendered = render_prompt_with_attachments(
            "look",
            &[
                attachment("image-1", AttachmentKind::Image, "/a/one.png", "one.png"),
                attachment("text-1", AttachmentKind::Text, "/a/notes.txt", "notes.txt"),
            ],
        );
        assert_eq!(
            rendered,
            "look\n\n---\nAttached files (read them):\n\
             image-1: /a/one.png\n\
             text-1: /a/notes.txt"
        );
    }
}
