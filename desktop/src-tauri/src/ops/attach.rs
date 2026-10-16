//! Files a caller attaches to a send, handled as the compose bar handles a
//! dropped file: copied into the project's `attachments/` dir, classified and
//! labelled (`image-1`, `text-1`, `file-1`), and handed to every recipient as
//! the same list, so the agent gets each copy's path after the message and the
//! window shows the attachments on the row.
//!
//! **Copy only after the window has the project open.** Opening a project
//! deletes every file in its `attachments/` dir that no journaled send and no
//! unsent draft refers to (`reclaim_project_attachments_impl`), and a send is
//! journaled only when its turn starts. A copy made before the window opened
//! the project would be deleted by that pass and the agent handed a path to
//! nothing. The pass runs at most once per project per process, before the
//! window lists the project's agents, and `ensure_ready` returns only after the
//! window has loaded the project, so a copy made after it cannot be reclaimed.
//! A send refused after the copy (a recipient turned busy, the message grew
//! too large) leaves its copies unreferenced; the next start's pass reclaims
//! them, as it does a file dropped into the compose bar and never sent.

use std::path::{Path, PathBuf};

use switchboard_core::{Attachment, AttachmentKind, ProjectId};

use crate::commands::stage_attachment_impl;
use crate::error::AppError;
use crate::state::AppState;

/// Extensions the window shows as images, as `desktop/src/lib/attachments.ts`
/// lists them. A test keeps the two lists equal.
const IMAGE_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "webp", "svg", "bmp", "heic", "heif", "avif", "ico", "tiff",
];

/// Extensions the window shows as text, as `desktop/src/lib/attachments.ts`
/// lists them. A test keeps the two lists equal.
const TEXT_EXTENSIONS: &[&str] = &[
    "txt", "md", "markdown", "json", "jsonl", "csv", "tsv", "log", "yaml", "yml", "toml", "xml",
    "html", "css", "rs", "ts", "tsx", "js", "jsx", "py", "go", "rb", "java", "kt", "c", "h", "cpp",
    "hpp", "sh", "bash", "zsh", "sql", "ini", "env", "svelte", "vue",
];

/// Refuse a send whose attachments cannot be read, before anything is
/// claimed or dispatched: each must be an absolute path to a file on this Mac.
pub(super) fn check_sources(sources: &[PathBuf]) -> Result<(), AppError> {
    for source in sources {
        if !source.is_absolute() {
            return Err(AppError::InvalidArgument(format!(
                "attachment {} is not an absolute path",
                source.display()
            )));
        }
        if !source.is_file() {
            return Err(AppError::InvalidArgument(format!(
                "attachment {} is not a file on this Mac",
                source.display()
            )));
        }
    }
    Ok(())
}

/// Copy each source into the project's `attachments/` dir and describe the
/// copies, labelled in the order given. Call only after the window has the
/// project open (see the module doc).
pub(super) async fn stage(
    state: &AppState,
    project_id: ProjectId,
    sources: &[PathBuf],
) -> Result<Vec<Attachment>, AppError> {
    let mut attachments: Vec<Attachment> = Vec::with_capacity(sources.len());
    for source in sources {
        let staged = stage_attachment_impl(state, project_id, source).await?;
        let kind = kind_of(source);
        attachments.push(Attachment {
            label: next_label(kind, &attachments),
            kind,
            path: staged.path,
            dispatched_path: None,
            original_name: staged.original_name,
        });
    }
    Ok(attachments)
}

/// The window's classification of a file by its extension.
fn kind_of(path: &Path) -> AttachmentKind {
    let extension = path
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.rsplit_once('.'))
        .map(|(_, extension)| extension.to_ascii_lowercase())
        .unwrap_or_default();
    if IMAGE_EXTENSIONS.contains(&extension.as_str()) {
        AttachmentKind::Image
    } else if TEXT_EXTENSIONS.contains(&extension.as_str()) {
        AttachmentKind::Text
    } else {
        AttachmentKind::File
    }
}

/// The next label for `kind` after the ones already given in this send.
fn next_label(kind: AttachmentKind, existing: &[Attachment]) -> String {
    let prefix = match kind {
        AttachmentKind::Image => "image",
        AttachmentKind::Text => "text",
        _ => "file",
    };
    let count = existing.iter().filter(|a| a.kind == kind).count();
    format!("{prefix}-{}", count + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_are_classified_and_numbered_per_kind_in_the_order_given() {
        let names = [
            "/tmp/a.png",
            "/tmp/b.PNG",
            "/tmp/run.log",
            "/tmp/.env",
            "/tmp/archive.tar.gz",
            "/tmp/README",
        ];
        let mut attachments: Vec<Attachment> = Vec::new();
        for name in names {
            let kind = kind_of(Path::new(name));
            attachments.push(Attachment {
                label: next_label(kind, &attachments),
                kind,
                path: name.to_owned(),
                dispatched_path: None,
                original_name: name.to_owned(),
            });
        }
        let labels: Vec<&str> = attachments.iter().map(|a| a.label.as_str()).collect();
        assert_eq!(
            labels,
            ["image-1", "image-2", "text-1", "text-2", "file-1", "file-2"]
        );
    }

    /// The window classifies dropped files with its own lists. A file the
    /// orchestrator attaches must get the same kind, or the same screenshot
    /// would show as an image when dropped and as a plain file when sent.
    #[test]
    fn the_extension_lists_match_the_windows() {
        let source = include_str!("../../../src/lib/attachments.ts");
        let set = |name: &str| -> Vec<String> {
            let start = source
                .find(&format!("const {name} = new Set(["))
                .unwrap_or_else(|| panic!("{name} is in attachments.ts"));
            let body = &source[start..];
            let body = &body[body.find('[').unwrap() + 1..body.find(']').unwrap()];
            let mut extensions: Vec<String> = body
                .split(',')
                .map(|item| item.trim().trim_matches('"').to_owned())
                .filter(|item| !item.is_empty())
                .collect();
            extensions.sort();
            extensions
        };
        let sorted = |list: &[&str]| -> Vec<String> {
            let mut list: Vec<String> = list.iter().map(|s| (*s).to_owned()).collect();
            list.sort();
            list
        };
        assert_eq!(set("IMAGE_EXTENSIONS"), sorted(IMAGE_EXTENSIONS));
        assert_eq!(set("TEXT_EXTENSIONS"), sorted(TEXT_EXTENSIONS));
    }
}
