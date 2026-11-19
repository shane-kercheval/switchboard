//! Decides whether a Codex run passes `--trust`, which saves trust for the
//! run's folder in the user's `$CODEX_HOME/config.toml` when the folder has no
//! saved decision, so the folder's `.codex/config.toml` loads.
//!
//! With `--trust`, Codex refuses the whole run (exit 1, no events) in two
//! cases, so the flag is left off for them and the run goes ahead without the
//! project config. Probed @ codex-cli 0.170.0.
//! - The user marked the folder untrusted ("<path> is marked as untrusted").
//!   Codex reads the entries for the folder, its project root (the nearest
//!   folder holding a `project_root_markers` entry, `.git` by default), its git
//!   repository root (the nearest folder holding `.git` whatever the markers
//!   say, or a linked worktree's main checkout), and folders holding `.codex/`
//!   between the first two. This check reads every folder from the run's folder
//!   up to the project root, and the repository root. An `untrusted` entry on
//!   any of them leaves the flag off, whatever order Codex reads them in: when
//!   another entry trusts the folder, its project config loads without the
//!   flag. An entry on a folder between the run's folder and the project root
//!   that holds no `.codex/` leaves the flag off where Codex would have run.
//! - Codex can't write the config ("failed to persist config"). Codex follows a
//!   symlinked config link by link, creates missing folders, and writes a
//!   temporary file beside the end of the chain, so the OS is asked whether
//!   that folder, or the nearest one above it that exists, is writable:
//!   permission bits miss a folder another user owns.
//!
//! Only `$CODEX_HOME/config.toml` is read. An `untrusted` entry or a
//! `project_root_markers` value from `/etc/codex/config.toml` or managed config
//! is missed, and Codex then refuses the run with its own message.

use std::path::{Path, PathBuf};

use super::own_server::CodexConfigSources;

/// Largest `.git` file read when looking for a linked worktree's main
/// checkout; Codex applies the same cap.
const MAX_GIT_FILE_BYTES: u64 = 64 * 1024;

/// Codex's `project_root_markers` when the config sets none.
const DEFAULT_PROJECT_ROOT_MARKERS: [&str; 1] = [".git"];

/// Whether a Codex run in `cwd` passes `--trust`: false when the user's Codex
/// config marks the folder untrusted or can't be written.
#[must_use]
pub(crate) fn should_pass_codex_trust(
    sources: &CodexConfigSources,
    home_dir: &Path,
    cwd: &Path,
) -> bool {
    let file = sources.user_file(home_dir, cwd);
    if let Some(key) = untrusted_entry(&file, cwd) {
        tracing::info!(
            entry = %key,
            "Codex marks this folder untrusted; running Codex without --trust"
        );
        return false;
    }
    if let Some(folder) = unwritable_folder(&file) {
        tracing::info!(
            folder = %folder.display(),
            "Codex can't save trust here; running Codex without --trust"
        );
        return false;
    }
    true
}

/// The key of an `untrusted` entry in `file` that names `cwd`, a folder above
/// it up to its project root, or its git repository root. A file that is
/// missing or can't be read or parsed has none.
fn untrusted_entry(file: &Path, cwd: &Path) -> Option<String> {
    let text = std::fs::read_to_string(file).ok()?;
    let config: toml::Table = toml::from_str(&text).ok()?;
    let projects = config.get("projects")?.as_table()?;
    let untrusted: Vec<&String> = projects
        .iter()
        .filter(|(_, entry)| {
            entry.get("trust_level").and_then(toml::Value::as_str) == Some("untrusted")
        })
        .map(|(key, _)| key)
        .collect();
    if untrusted.is_empty() {
        return None;
    }

    let markers = project_root_markers(&config);
    let canonical = std::fs::canonicalize(cwd).ok();
    let mut folders = up_to_project_root(cwd, &markers);
    if let Some(canonical) = &canonical {
        folders.extend(up_to_project_root(canonical, &markers));
    }
    if let Some(root) = repo_root(canonical.as_deref().unwrap_or(cwd)) {
        folders.push(root);
    }
    untrusted
        .into_iter()
        .find(|key| {
            let key = Path::new(key.as_str());
            folders.iter().any(|folder| folder == key)
                || std::fs::canonicalize(key).is_ok_and(|key| folders.contains(&key))
        })
        .cloned()
}

/// The config's `project_root_markers`, or Codex's default when it sets none.
/// A value that isn't a list of strings reads as the default; Codex refuses to
/// start with one.
fn project_root_markers(config: &toml::Table) -> Vec<String> {
    let custom = match config.get("project_root_markers") {
        Some(toml::Value::Array(entries)) => entries
            .iter()
            .map(|entry| entry.as_str().map(str::to_owned))
            .collect::<Option<Vec<String>>>(),
        _ => None,
    };
    custom.unwrap_or_else(|| DEFAULT_PROJECT_ROOT_MARKERS.map(str::to_owned).to_vec())
}

/// `dir` and each folder above it up to its project root, the nearest one
/// holding a marker. Outside any project that is `dir` alone, as Codex treats
/// it.
fn up_to_project_root(dir: &Path, markers: &[String]) -> Vec<PathBuf> {
    let mut folders = Vec::new();
    for folder in dir.ancestors() {
        folders.push(folder.to_path_buf());
        if holds_marker(folder, markers) {
            return folders;
        }
    }
    vec![dir.to_path_buf()]
}

/// Whether `folder` holds one of `markers`. Codex skips a `.git` folder with no
/// `HEAD`.
fn holds_marker(folder: &Path, markers: &[String]) -> bool {
    markers.iter().any(|marker| {
        let path = folder.join(marker);
        std::fs::metadata(&path).is_ok_and(|metadata| {
            marker != ".git" || !metadata.is_dir() || path.join("HEAD").exists()
        })
    })
}

/// The git repository root Codex reads an entry for, whatever
/// `project_root_markers` says: the nearest folder above `dir` holding `.git`,
/// skipping a `.git` folder with no `HEAD`, or for a linked worktree its main
/// checkout. A worktree's `.git` is a file whose `gitdir:` names
/// `<main>/.git/worktrees/<name>`.
fn repo_root(dir: &Path) -> Option<PathBuf> {
    let root = dir.ancestors().find(|folder| {
        let dot_git = folder.join(".git");
        std::fs::metadata(&dot_git)
            .is_ok_and(|metadata| !metadata.is_dir() || dot_git.join("HEAD").exists())
    })?;
    let dot_git = root.join(".git");
    if dot_git.is_dir() {
        return Some(root.to_path_buf());
    }
    let metadata = std::fs::symlink_metadata(&dot_git).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_GIT_FILE_BYTES {
        return None;
    }
    let text = std::fs::read_to_string(&dot_git).ok()?;
    let git_dir = text
        .lines()
        .find_map(|line| line.strip_prefix("gitdir:"))?
        .trim();
    let git_dir = std::fs::canonicalize(root.join(git_dir)).ok()?;
    let worktrees = git_dir.parent()?;
    if worktrees.file_name()? != "worktrees" {
        return None;
    }
    Some(worktrees.parent()?.parent()?.to_path_buf())
}

/// The folder Codex would write `file` through, when the OS says this process
/// can't write there.
#[cfg(unix)]
fn unwritable_folder(file: &Path) -> Option<PathBuf> {
    let target = write_target(file);
    let folder = target
        .parent()?
        .ancestors()
        .find(|folder| folder.exists())?;
    let folder = std::fs::canonicalize(folder).ok()?;
    nix::unistd::access(folder.as_path(), nix::unistd::AccessFlags::W_OK)
        .is_err()
        .then_some(folder)
}

/// Where Codex writes `file`: the end of its symlink chain, which may not exist
/// yet. Codex writes a chain that loops or can't be read at `file` itself.
#[cfg(unix)]
fn write_target(file: &Path) -> PathBuf {
    let mut current = file.to_path_buf();
    let mut visited = std::collections::HashSet::new();
    loop {
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {}
            Ok(_) => return current,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return current,
            Err(_) => return file.to_path_buf(),
        }
        if !visited.insert(current.clone()) {
            return file.to_path_buf();
        }
        let Ok(link) = std::fs::read_link(&current) else {
            return file.to_path_buf();
        };
        current = match current.parent() {
            Some(parent) if link.is_relative() => parent.join(link),
            _ => link,
        };
    }
}

#[cfg(not(unix))]
fn unwritable_folder(_file: &Path) -> Option<PathBuf> {
    None
}

/// Git repositories for tests, made without the developer's or the system's
/// git config so a hook or signing setting can't fail them.
#[cfg(test)]
pub(super) mod test_git {
    use std::path::Path;
    use std::process::{Command, Stdio};

    pub(crate) fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args([
                "-c",
                "user.name=test",
                "-c",
                "user.email=test@example.invalid",
            ])
            .args(["-c", "init.defaultBranch=main"])
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdout(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed in {}", dir.display());
    }

    /// A repository at `main` with one commit, and a linked worktree of it at
    /// `worktree`.
    pub(crate) fn repo_with_worktree(main: &Path, worktree: &Path) {
        std::fs::create_dir_all(main).unwrap();
        git(main, &["init", "-q"]);
        git(main, &["commit", "-q", "--allow-empty", "-m", "init"]);
        git(main, &["worktree", "add", "-q", worktree.to_str().unwrap()]);
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{PermissionsExt, symlink};

    use tempfile::TempDir;

    use super::test_git::{git, repo_with_worktree};
    use super::*;

    /// Sources whose `CODEX_HOME` is `home` and whose system file is absent.
    fn decide(home: &Path, cwd: &Path) -> bool {
        let sources = CodexConfigSources {
            system_file: home.join("absent-system-config.toml"),
            codex_home: Some(home.as_os_str().to_owned()),
        };
        should_pass_codex_trust(&sources, Path::new("/nonexistent-home"), cwd)
    }

    fn untrusted(folder: &Path) -> String {
        format!(
            "[projects.\"{}\"]\ntrust_level = \"untrusted\"\n",
            folder.display()
        )
    }

    fn trusted(folder: &Path) -> String {
        format!(
            "[projects.\"{}\"]\ntrust_level = \"trusted\"\n",
            folder.display()
        )
    }

    /// A Codex home, and a canonical folder to run in.
    fn setup() -> (TempDir, TempDir, PathBuf) {
        let home = TempDir::new().unwrap();
        let work = TempDir::new().unwrap();
        let folder = work.path().canonicalize().unwrap();
        (home, work, folder)
    }

    fn write_config(home: &Path, text: &str) {
        std::fs::write(home.join("config.toml"), text).unwrap();
    }

    #[test]
    fn passes_trust_when_there_is_no_config_file() {
        let (home, _work, folder) = setup();
        assert!(decide(home.path(), &folder));
    }

    #[test]
    fn passes_trust_when_no_entry_names_the_folder() {
        let (home, work, folder) = setup();
        let sibling = work.path().canonicalize().unwrap().join("sibling");
        let cwd = folder.join("repo");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(&sibling).unwrap();
        write_config(
            home.path(),
            &format!(
                "model = \"gpt-6-luna\"\n{}{}",
                untrusted(&sibling),
                trusted(&cwd)
            ),
        );
        assert!(decide(home.path(), &cwd));
    }

    #[test]
    fn omits_trust_for_a_folder_marked_untrusted() {
        let (home, _work, folder) = setup();
        write_config(home.path(), &untrusted(&folder));
        assert!(!decide(home.path(), &folder));
    }

    #[test]
    fn omits_trust_when_the_git_root_above_the_folder_is_marked_untrusted() {
        let (home, _work, folder) = setup();
        git(&folder, &["init", "-q"]);
        let cwd = folder.join("a/b");
        std::fs::create_dir_all(&cwd).unwrap();
        write_config(home.path(), &untrusted(&folder));
        assert!(!decide(home.path(), &cwd));
    }

    #[test]
    fn passes_trust_when_a_folder_above_the_git_root_is_marked_untrusted() {
        // Codex reads no entry above the project root, so it saves trust for
        // the repository.
        let (home, _work, folder) = setup();
        let repo = folder.join("repo");
        std::fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        write_config(home.path(), &untrusted(&folder));
        assert!(decide(home.path(), &repo));
    }

    #[test]
    fn passes_trust_when_a_folder_above_a_folder_outside_any_project_is_marked_untrusted() {
        let (home, _work, folder) = setup();
        let cwd = folder.join("a/b");
        std::fs::create_dir_all(&cwd).unwrap();
        write_config(home.path(), &untrusted(&folder));
        assert!(decide(home.path(), &cwd));
    }

    #[test]
    fn reads_project_root_markers_from_the_config() {
        let (home, _work, folder) = setup();
        std::fs::create_dir(folder.join(".hg")).unwrap();
        let cwd = folder.join("a");
        std::fs::create_dir(&cwd).unwrap();
        write_config(
            home.path(),
            &format!("project_root_markers = [\".hg\"]\n{}", untrusted(&folder)),
        );
        assert!(!decide(home.path(), &cwd));
    }

    #[test]
    fn omits_trust_when_custom_markers_leave_out_an_untrusted_git_root() {
        // Codex reads the git root's entry whatever the markers say.
        let (home, _work, folder) = setup();
        git(&folder, &["init", "-q"]);
        let cwd = folder.join("sub");
        std::fs::create_dir(&cwd).unwrap();
        write_config(
            home.path(),
            &format!("project_root_markers = [\".hg\"]\n{}", untrusted(&folder)),
        );
        assert!(!decide(home.path(), &cwd));
    }

    #[test]
    fn a_git_folder_without_head_is_not_a_project_root() {
        let (home, _work, folder) = setup();
        std::fs::create_dir(folder.join(".git")).unwrap();
        let cwd = folder.join("a");
        std::fs::create_dir(&cwd).unwrap();
        write_config(home.path(), &untrusted(&folder));
        assert!(decide(home.path(), &cwd));
    }

    #[test]
    fn reads_a_single_quoted_key() {
        let (home, _work, folder) = setup();
        write_config(
            home.path(),
            &format!(
                "[projects.'{}']\ntrust_level = \"untrusted\"\n",
                folder.display()
            ),
        );
        assert!(!decide(home.path(), &folder));
    }

    #[test]
    fn matches_a_key_written_through_a_symlink() {
        let (home, work, folder) = setup();
        let link = work.path().join("link");
        let real = folder.join("real");
        std::fs::create_dir(&real).unwrap();
        symlink(&real, &link).unwrap();
        write_config(home.path(), &untrusted(&link));
        assert!(!decide(home.path(), &real));
    }

    #[test]
    fn matches_a_folder_reached_through_a_symlink() {
        let (home, work, folder) = setup();
        let link = work.path().join("link");
        let real = folder.join("real");
        std::fs::create_dir_all(real.join("sub")).unwrap();
        git(&real, &["init", "-q"]);
        symlink(&real, &link).unwrap();
        write_config(home.path(), &untrusted(&real));
        assert!(!decide(home.path(), &link.join("sub")));
    }

    #[test]
    fn omits_trust_when_a_linked_worktrees_main_checkout_is_untrusted() {
        let (home, _work, folder) = setup();
        let main = folder.join("main");
        let worktree = folder.join("wt");
        repo_with_worktree(&main, &worktree);
        std::fs::create_dir(worktree.join("src")).unwrap();
        write_config(home.path(), &untrusted(&main));
        assert!(!decide(home.path(), &worktree.join("src")));
    }

    #[test]
    fn passes_trust_when_the_config_is_not_toml() {
        let (home, _work, folder) = setup();
        write_config(home.path(), &format!("{}not = [valid", untrusted(&folder)));
        assert!(decide(home.path(), &folder));
    }

    #[test]
    fn passes_trust_for_a_read_only_config_in_a_writable_folder() {
        // Codex replaces the file through a temporary one, so only the
        // folder's permission matters.
        let (home, _work, folder) = setup();
        write_config(home.path(), "model = \"gpt-6-luna\"\n");
        let file = home.path().join("config.toml");
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o444)).unwrap();
        assert!(decide(home.path(), &folder));
    }

    #[test]
    fn passes_trust_when_codex_home_does_not_exist_yet() {
        // Codex creates the missing folders, so the nearest one that exists
        // decides.
        let (home, _work, folder) = setup();
        assert!(decide(&home.path().join("not-yet/codex-home"), &folder));
    }

    #[test]
    fn omits_trust_when_the_config_links_into_a_read_only_folder() {
        let (home, work, folder) = setup();
        let store = work.path().join("store");
        std::fs::create_dir(&store).unwrap();
        std::fs::write(store.join("config.toml"), "model = \"gpt-6-luna\"\n").unwrap();
        symlink(store.join("config.toml"), home.path().join("config.toml")).unwrap();
        std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o555)).unwrap();
        let decision = decide(home.path(), &folder);
        std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(!decision);
    }

    #[test]
    fn omits_trust_when_the_config_is_a_dangling_link_into_a_read_only_folder() {
        // Codex writes at the link's missing target, not beside the link.
        let (home, work, folder) = setup();
        let store = work.path().join("store");
        std::fs::create_dir(&store).unwrap();
        symlink(store.join("config.toml"), home.path().join("config.toml")).unwrap();
        std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o555)).unwrap();
        let decision = decide(home.path(), &folder);
        std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(!decision);
    }

    #[test]
    fn omits_trust_when_the_config_links_into_a_folder_another_user_owns() {
        // `/etc` is root's, with mode 755: it has a write bit, but not for
        // this user.
        let (home, _work, folder) = setup();
        symlink("/etc/hosts", home.path().join("config.toml")).unwrap();
        assert!(!decide(home.path(), &folder));
    }
}
