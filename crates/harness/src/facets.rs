//! Normalized tool-call facets.
//!
//! Each harness speaks its own tool vocabulary (Claude's `Edit`, Codex's
//! `file_change`/`apply_patch`, Antigravity's `replace_file_content`, …).
//! A [`ToolFacet`] describes *what kind of operation* a tool call is,
//! independent of that vocabulary, so the frontend can render one stable,
//! scannable verb per operation across all harnesses. Classification happens
//! once per harness, in Rust, where it is testable — never in a frontend
//! `switch (tool.name)`.
//!
//! The facet is **additive**: the raw `name` and `input` ride alongside it
//! unchanged on every event, and remain the provenance escape hatch. A tool
//! we have not mapped (including Claude's `Task` subagent dispatch,
//! deliberately unmapped) degrades to [`ToolFacet::Other`] and renders
//! through the generic path.
//!
//! Observed per-harness vocabularies and shapes: `docs/harness-behavior.md`
//! §3.6.

use serde::{Deserialize, Serialize};

/// Byte cap applied to each content-bearing facet string (`Write::content`,
/// [`EditPair`] sides). Prevents the facet from *duplicating* a large payload
/// on the wire — it does **not** bound the event, because the raw `input`
/// rides alongside uncapped; bounding what the renderer does with that is the
/// renderer's job.
pub const FACET_CONTENT_CAP: usize = 256 * 1024;

/// What kind of operation a tool call is. Serde-tagged like the surrounding
/// wire types; `#[non_exhaustive]` so a new operation kind is an additive
/// variant, not a wire break — TS consumers must default unknown
/// `facet_kind`s to the `Other` rendering path.
///
/// Contract notes that every consumer can rely on:
///
/// - **Paths are absolute** wherever the harness supplies them (probe result:
///   every harness already emits absolute paths — §3.6). Adapters with a
///   known cwd resolve relative spellings lexically before constructing the
///   facet; there is exactly one path field per file, and a project-relative
///   *display* path is derived at render time, never carried on the wire.
/// - **No line numbers.** Neither Claude's `Edit` input nor Codex's patches
///   carry absolute file positions, so edit rendering is snippet-scoped: the
///   reader sees the change, not its location. Deliberate accepted
///   limitation.
/// - The facet is computed at tool *start*; failure is carried by
///   `ToolCompleted::is_error`, so `Shell` carries no exit code.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "facet_kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ToolFacet {
    /// One or more files changed in place. A *list* of files because Codex's
    /// `apply_patch` touches several in one call; a list of [`EditPair`]s per
    /// file because one call can make several changes to the same file.
    /// Claude's `Edit` is the degenerate case: one file, one pair.
    Edit {
        files: Vec<EditedFile>,
    },
    /// A whole file written. Distinct from `Edit` even though a write is
    /// arguably an edit with an empty "before": the harness gives us the new
    /// content but *not* the prior content. The UI intentionally infers
    /// creation because dedicated writes overwhelmingly create files,
    /// accepting that rare overwrites appear as all-added diffs.
    Write {
        path: String,
        content: String,
        truncated: bool,
    },
    Read {
        path: String,
    },
    Shell {
        command: String,
        cwd: Option<String>,
    },
    Search {
        pattern: String,
        path: Option<String>,
    },
    Todo {
        items: Vec<TodoItem>,
    },
    Mcp {
        server: String,
        tool: String,
    },
    /// Graceful degradation for any unmapped tool. Renders via the generic
    /// raw-input path.
    Other,
}

/// One file touched by an [`ToolFacet::Edit`].
///
/// `edits` **empty means content-unavailable**, not "no change": Codex's live
/// `file_change` item announces paths and change-kinds with no content (the
/// content exists only in its session file), so a live Codex edit facet
/// carries the file list with empty pairs and is upgraded once the turn's
/// post-terminal session-file read supplies the patch (§3.6).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EditedFile {
    /// Absolute, normalized — see the path contract on [`ToolFacet`].
    pub path: String,
    pub change: EditChange,
    pub edits: Vec<EditPair>,
    pub truncated: bool,
}

/// How an [`EditedFile`] changed. `#[non_exhaustive]`: an unrecognized
/// harness change-kind maps to `Modified` (the least-wrong reading — the
/// path was touched) rather than failing the event.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum EditChange {
    Added,
    Modified,
    Deleted,
}

/// One before/after pair within a file. An added file is `old: ""`; a
/// deleted file is `new: ""`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EditPair {
    pub old: String,
    pub new: String,
}

/// One checklist entry of a [`ToolFacet::Todo`]. `status` is the harness's
/// own vocabulary (`pending` / `in_progress` / `completed` / …), kept as an
/// opaque string so upstream additions pass through. `content` is best
/// effort: Claude's `TaskUpdate` carries only a task id and a status, so a
/// status-only update surfaces the id as its content.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TodoItem {
    pub content: String,
    pub status: String,
}

/// Cap `s` at [`FACET_CONTENT_CAP`] bytes (on a char boundary). Returns the
/// possibly-shortened string and whether truncation happened.
pub(crate) fn cap_content(s: &str) -> (String, bool) {
    if s.len() <= FACET_CONTENT_CAP {
        return (s.to_owned(), false);
    }
    let mut end = FACET_CONTENT_CAP;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    (s[..end].to_owned(), true)
}

/// Split a Claude/Gemini-convention MCP tool name (`mcp__<server>__<tool>`)
/// into `(server, tool)`. `None` when the name doesn't carry both parts —
/// callers fall back to [`ToolFacet::Other`] rather than guessing.
pub(crate) fn split_mcp_name(name: &str) -> Option<(String, String)> {
    let rest = name.strip_prefix("mcp__")?;
    let (server, tool) = rest.split_once("__")?;
    if server.is_empty() || tool.is_empty() {
        return None;
    }
    Some((server.to_owned(), tool.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn facet_serializes_with_facet_kind_tag() {
        let facet = ToolFacet::Shell {
            command: "ls".to_owned(),
            cwd: None,
        };
        let value = serde_json::to_value(&facet).unwrap();
        assert_eq!(value["facet_kind"], "shell");
        assert_eq!(value["command"], "ls");
        assert!(value["cwd"].is_null());
    }

    #[test]
    fn edit_facet_round_trips() {
        let facet = ToolFacet::Edit {
            files: vec![EditedFile {
                path: "/tmp/a.txt".to_owned(),
                change: EditChange::Modified,
                edits: vec![EditPair {
                    old: "foo".to_owned(),
                    new: "bar".to_owned(),
                }],
                truncated: false,
            }],
        };
        let value = serde_json::to_value(&facet).unwrap();
        assert_eq!(value["facet_kind"], "edit");
        assert_eq!(value["files"][0]["change"], "modified");
        let back: ToolFacet = serde_json::from_value(value).unwrap();
        assert_eq!(back, facet);
    }

    #[test]
    fn unknown_facet_kind_fails_deserialization_not_panics() {
        // The Rust side never receives foreign facets today (it only
        // produces them), but the tag must at least error cleanly.
        let result = serde_json::from_value::<ToolFacet>(json!({"facet_kind": "hologram"}));
        assert!(result.is_err());
    }

    #[test]
    fn cap_content_under_cap_is_unchanged() {
        let (s, truncated) = cap_content("hello");
        assert_eq!(s, "hello");
        assert!(!truncated);
    }

    #[test]
    fn cap_content_over_cap_truncates_on_char_boundary() {
        // Multi-byte char straddling the cap must not split.
        let body = "é".repeat(FACET_CONTENT_CAP); // 2 bytes each → over cap
        let (s, truncated) = cap_content(&body);
        assert!(truncated);
        assert!(s.len() <= FACET_CONTENT_CAP);
        assert!(s.chars().all(|c| c == 'é'));
    }

    #[test]
    fn split_mcp_name_extracts_server_and_tool() {
        assert_eq!(
            split_mcp_name("mcp__tiddly__search_items"),
            Some(("tiddly".to_owned(), "search_items".to_owned()))
        );
    }

    #[test]
    fn split_mcp_name_rejects_incomplete_names() {
        assert_eq!(split_mcp_name("mcp__loneserver"), None);
        assert_eq!(split_mcp_name("mcp____"), None);
        assert_eq!(split_mcp_name("Bash"), None);
    }
}
