//! First-run seeding of the bundled example prompts into the user-global
//! default prompts directory.
//!
//! The examples are baked into the binary (`include_str!`) so seeding works
//! identically in dev, tests, and the bundled app, with no resource-dir lookup.
//! Seeding is **first-run only**, gated on a one-time marker file — deleting a
//! seeded example never re-creates it, and an existing file is never
//! overwritten. This serves the zero-setup onboarding goal (a fresh install
//! ships a usable, editable example prompt) without fighting the user's edits.

use std::path::Path;

/// Marker recording that first-run seeding has happened. Its presence — not the
/// directory being non-empty — is what suppresses re-seeding, so a user who
/// deletes the example is not re-seeded on the next launch. Not a `.md` file, so
/// the local provider's scan ignores it.
const SEED_MARKER: &str = ".switchboard-seeded";

/// The example prompts shipped with the app, as `(filename, contents)`.
const EXAMPLE_PROMPTS: &[(&str, &str)] = &[(
    "code-review.md",
    include_str!("../resources/prompts/code-review.md"),
)];

/// Seed the example prompts into `prompts_dir` if they have never been seeded.
/// Best-effort: a failure is logged and does not block startup (prompts are
/// convenience state, and the user can still author their own).
pub fn seed_example_prompts(prompts_dir: &Path) {
    let marker = prompts_dir.join(SEED_MARKER);
    if marker.exists() {
        return;
    }
    if let Err(e) = write_examples(prompts_dir, &marker) {
        tracing::warn!(
            dir = %prompts_dir.display(),
            error = %e,
            "could not seed example prompts"
        );
    }
}

fn write_examples(prompts_dir: &Path, marker: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(prompts_dir)?;
    for (filename, contents) in EXAMPLE_PROMPTS {
        let path = prompts_dir.join(filename);
        // Never overwrite an existing file — respect the user's edits.
        if !path.exists() {
            std::fs::write(path, contents)?;
        }
    }
    std::fs::write(marker, b"")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn seeds_example_and_marker_on_first_run() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("prompts");

        seed_example_prompts(&dir);

        assert!(dir.join("code-review.md").is_file());
        assert!(dir.join(SEED_MARKER).is_file());
        let content = std::fs::read_to_string(dir.join("code-review.md")).unwrap();
        assert!(content.contains("name: code-review"));
    }

    #[test]
    fn does_not_reseed_a_deleted_example() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("prompts");

        seed_example_prompts(&dir);
        std::fs::remove_file(dir.join("code-review.md")).unwrap();

        // Marker still present → second run is a no-op, the deletion stands.
        seed_example_prompts(&dir);
        assert!(!dir.join("code-review.md").exists());
    }

    #[test]
    fn never_overwrites_an_existing_example() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("prompts");
        std::fs::create_dir_all(&dir).unwrap();
        // A user-edited example present before any marker exists.
        std::fs::write(dir.join("code-review.md"), "my edits").unwrap();

        seed_example_prompts(&dir);

        assert_eq!(
            std::fs::read_to_string(dir.join("code-review.md")).unwrap(),
            "my edits"
        );
        assert!(dir.join(SEED_MARKER).is_file());
    }
}
