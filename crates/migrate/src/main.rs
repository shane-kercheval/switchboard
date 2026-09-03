//! One-off migration of legacy `<dir>/.switchboard/` project state into the
//! user-global store. See [`lib`][switchboard_migrate] for the mechanics; this
//! binary is arg parsing, confirmation, and the report.

use std::io::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;

use switchboard_migrate::{MigrationReport, default_target_root, migrate, workspace_directories};

const USAGE: &str = "\
switchboard-migrate — copy legacy <dir>/.switchboard/ projects into the user-global store.

USAGE:
    cargo run -p switchboard-migrate [-- OPTIONS]

OPTIONS:
    --workspace-yaml <path>   The old workspace.yaml listing your working
                              directories. Default: the installed app's
                              (~/Library/Application Support/switchboard/workspace.yaml).
    --target-root <path>      Where the store is created. Default: the installed
                              app's store location. The target must not already
                              contain projects; to re-run, delete it first.
    --yes                     Skip the confirmation prompt.
    --help                    This text.

BEHAVIOR:
    Originals are never modified or deleted — the tool only reads your
    directories and writes a fresh store. If the result looks wrong, delete the
    target and re-run. Directories that are unavailable (unplugged disk, moved
    checkout) are reported and skipped; re-running after they return means
    deleting the target and migrating everything again.

    Attachment paths inside the migrated journals point at the target location
    permanently; the store must stay at the path it was written to.

    Run this BEFORE using the new app: the first time it saves its view-state
    (archiving a project) it rewrites workspace.yaml without the directory list
    this tool reads, and (once running) it may create an empty store at the
    default target. If that has already happened, a copy of the old
    workspace.yaml (or --workspace-yaml pointing at a backup) is the supported
    way to supply the directory list.";

fn main() -> ExitCode {
    let mut workspace_yaml: Option<PathBuf> = None;
    let mut target_root: Option<PathBuf> = None;
    let mut assume_yes = false;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            "--yes" => assume_yes = true,
            // A flag with no value is a usage error, never a silent fallback:
            // the documented recovery route is `--workspace-yaml <a preserved
            // copy>`, and falling back to the installed app's (possibly already
            // rewritten) file is the single worst thing to do with that input.
            "--workspace-yaml" | "--target-root" => {
                let Some(value) = args.next() else {
                    eprintln!("{arg} requires a path\n\n{USAGE}");
                    return ExitCode::FAILURE;
                };
                if arg == "--workspace-yaml" {
                    workspace_yaml = Some(PathBuf::from(value));
                } else {
                    target_root = Some(PathBuf::from(value));
                }
            }
            other => {
                eprintln!("unrecognized argument: {other}\n\n{USAGE}");
                return ExitCode::FAILURE;
            }
        }
    }

    let Some(workspace_yaml) = workspace_yaml.or_else(default_workspace_yaml) else {
        eprintln!("no config directory resolved and no --workspace-yaml given");
        return ExitCode::FAILURE;
    };
    let Some(target_root) = target_root.or_else(default_target_root) else {
        eprintln!("no config directory resolved and no --target-root given");
        return ExitCode::FAILURE;
    };

    let directories = match workspace_directories(&workspace_yaml) {
        Ok(dirs) => dirs,
        Err(e) => {
            eprintln!("could not read {}: {e}", workspace_yaml.display());
            return ExitCode::FAILURE;
        }
    };

    println!("Reading directories from: {}", workspace_yaml.display());
    println!("Writing the store to:     {}", target_root.display());
    println!("Directories to scan:      {}", directories.len());
    println!("Originals are not modified. To re-run, delete the target first.");
    println!(
        "NOTE: attachment paths inside the migrated journals will point at the target\n\
         location permanently — moving the store somewhere else afterwards breaks them.\n"
    );

    if !assume_yes {
        print!("Proceed? [y/N] ");
        let _ = std::io::stdout().flush();
        let mut answer = String::new();
        let _ = std::io::stdin().read_line(&mut answer);
        if !matches!(answer.trim(), "y" | "Y" | "yes") {
            println!("aborted; nothing written");
            return ExitCode::SUCCESS;
        }
    }

    match migrate(&directories, &target_root) {
        Ok(report) => {
            print_report(&report);
            let left = report
                .migrated
                .iter()
                .any(|m| !m.attachments_left.is_empty());
            if report.skipped.is_empty() && !left {
                ExitCode::SUCCESS
            } else {
                // Partial success. Exit 2 for a skipped directory *and* for an
                // attachment left behind — the latter is a file the app will
                // delete on first open, so a clean exit would be the same silent
                // loss this tool exists to make loud.
                ExitCode::from(2)
            }
        }
        Err(e) => {
            eprintln!("\nmigration failed: {e}");
            eprintln!("nothing in your working directories was modified.");
            eprintln!(
                "delete {} and re-run once the cause is fixed.",
                target_root.display()
            );
            ExitCode::FAILURE
        }
    }
}

fn default_workspace_yaml() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "switchboard")
        .map(|dirs| dirs.config_dir().join("workspace.yaml"))
}

fn print_report(report: &MigrationReport) {
    println!();
    for migrated in &report.migrated {
        println!(
            "migrated  {}  ({} project{})",
            migrated.directory.display(),
            migrated.projects.len(),
            if migrated.projects.len() == 1 {
                ""
            } else {
                "s"
            }
        );
        for (_, name, _) in &migrated.projects {
            println!("          - {name}");
        }
        println!(
            "          attachments: {} path(s) rewritten{}",
            migrated.attachments_rewritten,
            if migrated.attachments_left.is_empty() {
                String::new()
            } else {
                format!(", {} left untouched", migrated.attachments_left.len())
            }
        );
        for left in &migrated.attachments_left {
            println!(
                "          ! NOT MIGRATED — this file will be removed the first time the \n\
                 \x20           project is opened, because the journal still points at the \n\
                 \x20           old location: {left}"
            );
        }
    }
    for (path, reason) in &report.skipped {
        println!("SKIPPED   {}  ({reason})", path.display());
    }
    for path in &report.empty {
        println!("nothing   {}  (no legacy projects)", path.display());
    }
    println!(
        "\n{} project(s) migrated, {} directory(ies) skipped. Validation: all copied \
         projects re-opened and parsed cleanly.",
        report
            .migrated
            .iter()
            .map(|m| m.projects.len())
            .sum::<usize>(),
        report.skipped.len(),
    );
    if !report.skipped.is_empty() {
        println!(
            "To migrate a skipped directory later: make it available, DELETE the target \
             store, and re-run (the tool never merges into an existing store)."
        );
    }
}
