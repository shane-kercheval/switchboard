//! Codex tool-facet classification, including the `apply_patch` parser.
//!
//! Codex's edit story is split across its two channels (probe 2026-07-10 @
//! 0.143.0; `docs/harness-behavior.md` §3.6):
//!
//! - **Live stream**: a `file_change` item carries `changes: [{path, kind}]`
//!   — which files and how, but *no content*. [`file_change_facet`] builds an
//!   `Edit` facet with empty pair lists (= content unavailable).
//! - **Session file**: a `custom_tool_call` named `apply_patch` carries the
//!   full patch text. [`parse_apply_patch`] reconstructs per-file
//!   before/after strings so the frontend keeps one uniform `Edit` renderer —
//!   we deliberately normalize *into* before/after pairs rather than *out to*
//!   hunks, because the diff is computed lazily at render time and a single
//!   representation keeps one renderer.
//!
//! The adapter closes the live-content gap at turn end: the post-terminal
//! enrichment re-read (already performed every turn for usage/window) also
//! collects the turn's patch facets, and the producer emits
//! `ToolFacetUpdated` for each live `file_change` row (see `mod.rs`).
//!
//! Patch grammar parsed here (Codex "V4A", observed live):
//!
//! ```text
//! *** Begin Patch
//! *** Update File: /abs/path
//! @@ [optional context header]
//!  context line
//! -removed line
//! +added line
//! *** Add File: /abs/other
//! +file content lines
//! *** Delete File: /abs/gone
//! *** End Patch
//! ```

use std::path::Path;

use serde_json::Value;

use crate::facets::{EditChange, EditPair, EditedFile, ToolFacet, cap_content};

/// Facet for a live `file_change` item: files + change kinds, empty edit
/// pairs (content lives only in the session file). Returns `Other` when the
/// `changes` array is missing/empty — a content-free edit event with no
/// paths carries nothing renderable.
pub(crate) fn file_change_facet(item: &Value) -> ToolFacet {
    let Some(changes) = item.get("changes").and_then(Value::as_array) else {
        return ToolFacet::Other;
    };
    let files: Vec<EditedFile> = changes
        .iter()
        .filter_map(|c| {
            let path = c.get("path").and_then(Value::as_str)?;
            Some(EditedFile {
                path: path.to_owned(),
                change: change_kind(c.get("kind").and_then(Value::as_str).unwrap_or("")),
                edits: Vec::new(),
                truncated: false,
            })
        })
        .collect();
    if files.is_empty() {
        return ToolFacet::Other;
    }
    ToolFacet::Edit { files }
}

/// Facet for a session-file `function_call` named `exec_command`. Codex's
/// disk record carries the raw command (`cmd`) and shell cwd (`workdir`) —
/// unlike the live `command_execution`, whose `command` is the wrapped
/// `/bin/zsh -lc '…'` string (documented spelling divergence, §3.6).
pub(crate) fn exec_command_facet(arguments: &Value) -> ToolFacet {
    let Some(cmd) = arguments.get("cmd").and_then(Value::as_str) else {
        return ToolFacet::Other;
    };
    ToolFacet::Shell {
        command: cmd.to_owned(),
        cwd: arguments
            .get("workdir")
            .and_then(Value::as_str)
            .map(str::to_owned),
    }
}

/// Parse an `apply_patch` patch text into per-file before/after content.
///
/// `cwd` resolves relative section paths (observed paths are absolute, but
/// the patch grammar permits relative — resolve lexically, never touch the
/// filesystem). Returns `None` for text that isn't a patch (missing `***
/// Begin Patch`, or no file sections) — the caller degrades to `Other`
/// rather than fabricating an empty edit. Never panics on malformed input.
pub(crate) fn parse_apply_patch(patch: &str, cwd: Option<&Path>) -> Option<Vec<EditedFile>> {
    let mut lines = patch.lines().peekable();
    if lines.next()?.trim() != "*** Begin Patch" {
        return None;
    }

    let mut files: Vec<EditedFile> = Vec::new();
    while let Some(line) = lines.next() {
        if line.trim() == "*** End Patch" {
            break;
        }
        if let Some(path) = line.strip_prefix("*** Update File: ") {
            // `*** Move to:` (rename) is unobserved live; the facet keeps the
            // original path and the move target is visible in the raw input.
            if lines.peek().is_some_and(|l| l.starts_with("*** Move to: ")) {
                lines.next();
            }
            let mut edits: Vec<EditPair> = Vec::new();
            let mut truncated = false;
            // Hunks: `@@` opens one; -/+/context lines accumulate into the
            // old/new sides. A `*** ` line ends the section (checked before
            // consuming, so the outer loop sees it).
            let mut old: Vec<&str> = Vec::new();
            let mut new: Vec<&str> = Vec::new();
            let mut in_hunk = false;
            let flush = |old: &mut Vec<&str>, new: &mut Vec<&str>, truncated: &mut bool| {
                if old.is_empty() && new.is_empty() {
                    return None;
                }
                let (o, t1) = cap_content(&old.join("\n"));
                let (n, t2) = cap_content(&new.join("\n"));
                *truncated |= t1 || t2;
                old.clear();
                new.clear();
                Some(EditPair { old: o, new: n })
            };
            while let Some(&body) = lines.peek() {
                if body.starts_with("*** ") {
                    break;
                }
                lines.next();
                if body.starts_with("@@") {
                    edits.extend(flush(&mut old, &mut new, &mut truncated));
                    in_hunk = true;
                } else if !in_hunk {
                    // Body before any `@@` — not valid hunk content; skip.
                } else if let Some(removed) = body.strip_prefix('-') {
                    old.push(removed);
                } else if let Some(added) = body.strip_prefix('+') {
                    new.push(added);
                } else {
                    // Context line (leading space, or Codex's occasional bare
                    // spelling) belongs to both sides.
                    let ctx = body.strip_prefix(' ').unwrap_or(body);
                    old.push(ctx);
                    new.push(ctx);
                }
            }
            edits.extend(flush(&mut old, &mut new, &mut truncated));
            files.push(EditedFile {
                path: resolve_path(path, cwd),
                change: EditChange::Modified,
                edits,
                truncated,
            });
        } else if let Some(path) = line.strip_prefix("*** Add File: ") {
            let mut content: Vec<&str> = Vec::new();
            while let Some(added) = lines.peek().and_then(|l| l.strip_prefix('+')) {
                content.push(added);
                lines.next();
            }
            let (new, truncated) = cap_content(&content.join("\n"));
            files.push(EditedFile {
                path: resolve_path(path, cwd),
                change: EditChange::Added,
                edits: vec![EditPair {
                    old: String::new(),
                    new,
                }],
                truncated,
            });
        } else if let Some(path) = line.strip_prefix("*** Delete File: ") {
            // The patch does not carry the deleted content — empty pair list
            // means content-unavailable, same as the live channel.
            files.push(EditedFile {
                path: resolve_path(path, cwd),
                change: EditChange::Deleted,
                edits: Vec::new(),
                truncated: false,
            });
        }
        // Any other line at section level (including delimiter-lookalikes
        // that a section body already consumed) is skipped, not fatal.
    }

    if files.is_empty() { None } else { Some(files) }
}

/// `apply_patch` facet for a session-file `custom_tool_call`: parsed patch,
/// or `Other` when the input isn't parseable as a patch (raw input remains
/// the escape hatch — never fabricate an empty Edit).
pub(crate) fn apply_patch_facet(input: &str, cwd: Option<&Path>) -> ToolFacet {
    match parse_apply_patch(input, cwd) {
        Some(files) => ToolFacet::Edit { files },
        None => ToolFacet::Other,
    }
}

fn change_kind(kind: &str) -> EditChange {
    match kind {
        "add" => EditChange::Added,
        "delete" | "remove" => EditChange::Deleted,
        // Unknown kinds read as Modified — the path was touched (facets.rs
        // EditChange doc).
        _ => EditChange::Modified,
    }
}

fn resolve_path(path: &str, cwd: Option<&Path>) -> String {
    let p = Path::new(path);
    if p.is_absolute() {
        return path.to_owned();
    }
    match cwd {
        Some(base) => base.join(p).to_string_lossy().into_owned(),
        None => path.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn file_change_maps_paths_and_kinds_without_content() {
        let item = json!({"id": "item_4", "type": "file_change", "changes": [
            {"path": "/tmp/alpha.txt", "kind": "update"},
            {"path": "/tmp/zeta.txt", "kind": "add"}
        ], "status": "in_progress"});
        let ToolFacet::Edit { files } = file_change_facet(&item) else {
            panic!("expected Edit");
        };
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].change, EditChange::Modified);
        assert!(files[0].edits.is_empty(), "live facet carries no content");
        assert_eq!(files[1].change, EditChange::Added);
    }

    #[test]
    fn file_change_without_changes_degrades_to_other() {
        assert_eq!(file_change_facet(&json!({"id": "x"})), ToolFacet::Other);
        assert_eq!(file_change_facet(&json!({"changes": []})), ToolFacet::Other);
    }

    #[test]
    fn exec_command_maps_cmd_and_workdir() {
        let facet = exec_command_facet(
            &json!({"cmd": "git status", "workdir": "/tmp/w", "yield_time_ms": 1000}),
        );
        assert_eq!(
            facet,
            ToolFacet::Shell {
                command: "git status".to_owned(),
                cwd: Some("/tmp/w".to_owned())
            }
        );
    }

    #[test]
    fn single_file_modify_parses_to_one_pair() {
        // The exact shape captured live (fixtures/codex/apply-patch.session.jsonl).
        let patch =
            "*** Begin Patch\n*** Update File: /tmp/alpha.txt\n@@\n-foo\n+bar\n*** End Patch\n";
        let files = parse_apply_patch(patch, None).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "/tmp/alpha.txt");
        assert_eq!(files[0].change, EditChange::Modified);
        assert_eq!(
            files[0].edits,
            vec![EditPair {
                old: "foo".to_owned(),
                new: "bar".to_owned()
            }]
        );
    }

    #[test]
    fn multi_file_patch_yields_one_entry_per_file() {
        let patch = "*** Begin Patch\n*** Update File: /tmp/a.txt\n@@\n-x\n+y\n*** Add File: /tmp/b.txt\n+hello world\n*** End Patch\n";
        let files = parse_apply_patch(patch, None).unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].change, EditChange::Modified);
        assert_eq!(files[1].change, EditChange::Added);
        assert_eq!(files[1].edits[0].old, "");
        assert_eq!(files[1].edits[0].new, "hello world");
    }

    #[test]
    fn add_file_with_multiple_lines_joins_content() {
        let patch =
            "*** Begin Patch\n*** Add File: /tmp/c.txt\n+line one\n+line two\n*** End Patch\n";
        let files = parse_apply_patch(patch, None).unwrap();
        assert_eq!(files[0].edits[0].new, "line one\nline two");
    }

    #[test]
    fn delete_file_has_deleted_change_and_no_content() {
        let patch = "*** Begin Patch\n*** Delete File: /tmp/gone.txt\n*** End Patch\n";
        let files = parse_apply_patch(patch, None).unwrap();
        assert_eq!(files[0].change, EditChange::Deleted);
        assert!(files[0].edits.is_empty());
    }

    #[test]
    fn context_lines_land_on_both_sides() {
        let patch = "*** Begin Patch\n*** Update File: /tmp/a.txt\n@@ fn main\n keep\n-old\n+new\n keep2\n*** End Patch\n";
        let files = parse_apply_patch(patch, None).unwrap();
        assert_eq!(files[0].edits[0].old, "keep\nold\nkeep2");
        assert_eq!(files[0].edits[0].new, "keep\nnew\nkeep2");
    }

    #[test]
    fn multiple_hunks_yield_multiple_pairs() {
        let patch = "*** Begin Patch\n*** Update File: /tmp/a.txt\n@@ one\n-a\n+b\n@@ two\n-c\n+d\n*** End Patch\n";
        let files = parse_apply_patch(patch, None).unwrap();
        assert_eq!(files[0].edits.len(), 2);
    }

    #[test]
    fn added_content_resembling_a_delimiter_stays_content() {
        // A '+' line whose text looks like a patch delimiter must be
        // consumed as content, not terminate the section.
        let patch =
            "*** Begin Patch\n*** Add File: /tmp/d.txt\n+*** End Patch\n+more\n*** End Patch\n";
        let files = parse_apply_patch(patch, None).unwrap();
        assert_eq!(files[0].edits[0].new, "*** End Patch\nmore");
    }

    #[test]
    fn malformed_patch_degrades_to_none_not_panic() {
        assert!(parse_apply_patch("not a patch", None).is_none());
        assert!(parse_apply_patch("", None).is_none());
        assert!(parse_apply_patch("*** Begin Patch\ngarbage\n*** End Patch\n", None).is_none());
        assert_eq!(apply_patch_facet("echo hi", None), ToolFacet::Other);
    }

    #[test]
    fn relative_section_path_resolves_against_cwd() {
        let patch = "*** Begin Patch\n*** Update File: sub/rel.txt\n@@\n-a\n+b\n*** End Patch\n";
        let files = parse_apply_patch(patch, Some(Path::new("/work/dir"))).unwrap();
        assert_eq!(files[0].path, "/work/dir/sub/rel.txt");
    }

    #[test]
    fn move_to_line_is_consumed_and_path_stays_original() {
        let patch = "*** Begin Patch\n*** Update File: /tmp/a.txt\n*** Move to: /tmp/b.txt\n@@\n-x\n+y\n*** End Patch\n";
        let files = parse_apply_patch(patch, None).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "/tmp/a.txt");
        assert_eq!(files[0].edits.len(), 1);
    }

    #[test]
    fn oversized_hunk_sets_truncated() {
        let big = "x".repeat(crate::facets::FACET_CONTENT_CAP + 10);
        let patch = format!("*** Begin Patch\n*** Add File: /tmp/big.txt\n+{big}\n*** End Patch\n");
        let files = parse_apply_patch(&patch, None).unwrap();
        assert!(files[0].truncated);
        assert!(files[0].edits[0].new.len() <= crate::facets::FACET_CONTENT_CAP);
    }
}
