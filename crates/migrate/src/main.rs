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

    Run this BEFORE launching the new app: the app rewrites workspace.yaml into
    a shape that drops the legacy project cache, and (once running) may create
    an empty store at the default target.";

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
            "--workspace-yaml" => workspace_yaml = args.next().map(PathBuf::from),
            "--target-root" => target_root = args.next().map(PathBuf::from),
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
    println!("Originals are not modified. To re-run, delete the target first.\n");

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
            if report.skipped.is_empty() {
                ExitCode::SUCCESS
            } else {
                // Partial success: everything available migrated, but the exit
                // code says "look at the report" rather than reading as clean.
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
        for name in &migrated.projects {
            println!("          - {name}");
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
