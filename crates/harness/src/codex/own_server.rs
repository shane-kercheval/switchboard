//! Finds Switchboard's own MCP server in Codex's config files and builds the
//! `-c` overrides that turn it off for one run (see [`crate::own_mcp`]).
//!
//! Codex has no per-run deny list, so each matching entry is switched off with
//! `-c mcp_servers.<name>.enabled=false`. That override alone is unsafe: for a
//! name with no entry it leaves one with no transport, which fails the run with
//! "invalid transport in `mcp_servers.<name>`". So each override also repeats
//! the entry's own transport key with its own value (`url` or `command`). A
//! different transport key is not safe either: `-c url=…` over a stdio entry
//! fails with "url is not supported for stdio". Probed @ codex-cli 0.165.0.
//!
//! Only the files Codex reads on every run are read, lowest precedence first:
//! `/etc/codex/config.toml`, then `$CODEX_HOME/config.toml` (default
//! `~/.codex/config.toml`). The layers Codex applies between those files and
//! `-c` can add keys to an entry but not remove them, and `-c` merges key by
//! key, so the transport repeated here is one Codex's own entry already has.
//! Project `.codex/config.toml` files are not read: which of them Codex loads
//! depends on folder trust and the project root, and repeating a transport
//! from one Codex skipped could fail the run. An entry defined only in a
//! project file stays on.
//!
//! An override can still fail a run that would otherwise start. Codex applies
//! an admin's managed config (`/etc/codex/managed_config.toml`, or a macOS
//! device-management profile) after `-c`. When it forces the entry's
//! `enabled = true`, some layer sets `required = true`, a project's
//! `.codex/config.toml` points the entry at a working address, and the two
//! files read here point it at one that doesn't answer, the repeated transport
//! replaces the project's address and the required server fails to start. Read in the 0.165.0 loader
//! source, not probed.
//!
//! Where no override is safe, this passes none for the entry and logs a warning:
//! - a file that can't be read or parsed (no overrides at all, since the file
//!   could redefine any entry);
//! - an entry with both `url` and `command` (Codex rejects it anyway), with
//!   neither, with a non-string transport, or that isn't a table;
//! - a name outside `[A-Za-z0-9_-]`, which can't be written as a `-c` key path.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use crate::own_mcp::{OWN_MCP_SERVER_NAME, OwnMcpServer};

/// The system config file, read below the user's on every Codex run.
const SYSTEM_CONFIG_FILE: &str = "/etc/codex/config.toml";

/// Where the two config files the lookup reads are: the system file, and the
/// `CODEX_HOME` value the Codex child runs with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexConfigSources {
    pub system_file: PathBuf,
    pub codex_home: Option<OsString>,
}

impl CodexConfigSources {
    /// The system file Codex reads on macOS and Linux, and this process's
    /// `CODEX_HOME`, which every Codex child inherits.
    #[must_use]
    pub fn from_env() -> Self {
        Self {
            system_file: PathBuf::from(SYSTEM_CONFIG_FILE),
            codex_home: std::env::var_os("CODEX_HOME"),
        }
    }

    /// The two files for a run in `cwd`, lowest precedence first.
    fn files(&self, home_dir: &Path, cwd: &Path) -> [PathBuf; 2] {
        [
            self.system_file.clone(),
            codex_home(self.codex_home.as_deref(), home_dir, cwd).join("config.toml"),
        ]
    }
}

/// The `-c` arguments that turn Switchboard's own server off for a Codex run
/// in `cwd`, from the system file and `$CODEX_HOME/config.toml`.
#[must_use]
pub fn codex_own_server_overrides(
    own: OwnMcpServer,
    sources: &CodexConfigSources,
    home_dir: &Path,
    cwd: &Path,
) -> Vec<String> {
    overrides_from_files(&sources.files(home_dir, cwd), own)
}

/// Codex's home for a run in `cwd`: `CODEX_HOME` when set, resolved from `cwd`
/// when relative, as the Codex child started there resolves it; else
/// `<home>/.codex`.
fn codex_home(env_value: Option<&OsStr>, home_dir: &Path, cwd: &Path) -> PathBuf {
    match env_value.filter(|value| !value.is_empty()) {
        Some(value) => cwd.join(value),
        None => home_dir.join(".codex"),
    }
}

/// The overrides for the matching entries in `files`, lowest precedence first.
fn overrides_from_files(files: &[PathBuf], own: OwnMcpServer) -> Vec<String> {
    let Some(merged) = merged_servers(files) else {
        return Vec::new();
    };
    let mut args = Vec::new();
    for (name, entry) in &merged {
        if !is_own_entry(name, entry, own) {
            continue;
        }
        match transport_override(name, entry) {
            Ok((key, value)) => {
                args.push("-c".to_owned());
                args.push(format!(
                    "mcp_servers.{name}.{key}={}",
                    toml::Value::String(value.to_owned())
                ));
                args.push("-c".to_owned());
                args.push(format!("mcp_servers.{name}.enabled=false"));
            }
            Err(reason) => tracing::warn!(
                server = %name,
                reason,
                "not turning off Switchboard's own MCP server entry for this Codex run"
            ),
        }
    }
    args
}

/// The `[mcp_servers]` tables of `files` merged as Codex merges them, or
/// `None` when one can't be read or parsed.
fn merged_servers(files: &[PathBuf]) -> Option<BTreeMap<String, toml::Value>> {
    let mut merged: BTreeMap<String, toml::Value> = BTreeMap::new();
    for path in files {
        let Some(servers) = read_servers(path) else {
            tracing::warn!(
                path = %path.display(),
                "Codex config unreadable; not turning off Switchboard's own MCP server for this run"
            );
            return None;
        };
        for (name, entry) in servers {
            match merged.get_mut(&name) {
                Some(existing) => merge_into(existing, entry),
                None => {
                    merged.insert(name, entry);
                }
            }
        }
    }
    Some(merged)
}

/// The `[mcp_servers]` table of one file: empty when the file is absent or has
/// none, `None` when the file can't be read or parsed or the table isn't one.
fn read_servers(path: &Path) -> Option<toml::Table> {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Some(toml::Table::new()),
        Err(_) => return None,
    };
    let mut document: toml::Table = toml::from_str(&content).ok()?;
    match document.remove("mcp_servers") {
        None => Some(toml::Table::new()),
        Some(toml::Value::Table(servers)) => Some(servers),
        Some(_) => None,
    }
}

/// Tables merge key by key and anything else is replaced, as Codex layers
/// config files and `-c` overrides.
fn merge_into(base: &mut toml::Value, overlay: toml::Value) {
    match (base, overlay) {
        (toml::Value::Table(base), toml::Value::Table(overlay)) => {
            for (key, value) in overlay {
                match base.get_mut(&key) {
                    Some(existing) => merge_into(existing, value),
                    None => {
                        base.insert(key, value);
                    }
                }
            }
        }
        (base, overlay) => *base = overlay,
    }
}

/// Named `switchboard`, or reaching this instance's address: an HTTP entry's
/// `url`, or an argument of a stdio entry (an `mcp-remote` bridge).
fn is_own_entry(name: &str, entry: &toml::Value, own: OwnMcpServer) -> bool {
    if name == OWN_MCP_SERVER_NAME {
        return true;
    }
    let url = entry.get("url").and_then(toml::Value::as_str);
    let args = entry
        .get("args")
        .and_then(toml::Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    url.is_some_and(|url| own.is_own_url(url))
        || args
            .iter()
            .filter_map(toml::Value::as_str)
            .any(|arg| own.is_own_url(arg))
}

/// The transport key and value to repeat beside `enabled=false`, or why no
/// override is safe for this entry.
fn transport_override<'a>(
    name: &str,
    entry: &'a toml::Value,
) -> Result<(&'static str, &'a str), &'static str> {
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err("name has characters a -c key path can't carry");
    }
    let Some(table) = entry.as_table() else {
        return Err("entry is not a table");
    };
    match (table.get("url"), table.get("command")) {
        (Some(toml::Value::String(url)), None) => Ok(("url", url)),
        (None, Some(toml::Value::String(command))) => Ok(("command", command)),
        (Some(_), Some(_)) => Err("entry has both url and command"),
        (None, None) => Err("entry has neither url nor command"),
        _ => Err("entry's transport is not a string"),
    }
}

#[cfg(test)]
mod tests {
    use std::process::Stdio;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use switchboard_core::{AgentRecord, HarnessKind};
    use tempfile::TempDir;

    use super::*;

    const OWN: OwnMcpServer = OwnMcpServer { port: 47821 };

    fn write(path: &Path, content: &str) -> PathBuf {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
        path.to_owned()
    }

    fn overrides(files: &[PathBuf]) -> Vec<String> {
        overrides_from_files(files, OWN)
    }

    #[test]
    fn named_http_entry_gets_its_url_and_enabled_false() {
        let dir = TempDir::new().unwrap();
        let user = write(
            &dir.path().join("config.toml"),
            r#"
[mcp_servers.switchboard]
url = "http://127.0.0.1:47821/mcp"
bearer_token_env_var = "SB_TOKEN"

[mcp_servers.other]
url = "https://example.com/mcp"
"#,
        );

        assert_eq!(
            overrides(&[user]),
            vec![
                "-c",
                r#"mcp_servers.switchboard.url="http://127.0.0.1:47821/mcp""#,
                "-c",
                "mcp_servers.switchboard.enabled=false",
            ]
        );
    }

    #[test]
    fn named_stdio_entry_gets_its_command() {
        let dir = TempDir::new().unwrap();
        let user = write(
            &dir.path().join("config.toml"),
            r#"
[mcp_servers.switchboard]
command = "npx"
args = ["-y", "mcp-remote", "http://127.0.0.1:47821/mcp"]
"#,
        );

        assert_eq!(
            overrides(&[user]),
            vec![
                "-c",
                r#"mcp_servers.switchboard.command="npx""#,
                "-c",
                "mcp_servers.switchboard.enabled=false",
            ]
        );
    }

    #[test]
    fn renamed_entries_at_this_address_are_matched_in_either_host_form() {
        let dir = TempDir::new().unwrap();
        let user = write(
            &dir.path().join("config.toml"),
            r#"
[mcp_servers.sb_http]
url = "http://localhost:47821/mcp"

[mcp_servers.sb-bridge]
command = "npx"
args = ["-y", "mcp-remote", "http://127.0.0.1:47821/mcp", "--header", "Authorization: Bearer x"]

[mcp_servers.other_instance]
url = "http://127.0.0.1:47822/mcp"
"#,
        );

        assert_eq!(
            overrides(&[user]),
            vec![
                "-c",
                r#"mcp_servers.sb-bridge.command="npx""#,
                "-c",
                "mcp_servers.sb-bridge.enabled=false",
                "-c",
                r#"mcp_servers.sb_http.url="http://localhost:47821/mcp""#,
                "-c",
                "mcp_servers.sb_http.enabled=false",
            ]
        );
    }

    #[test]
    fn values_are_written_as_escaped_toml_strings() {
        let dir = TempDir::new().unwrap();
        let user = write(
            &dir.path().join("config.toml"),
            r#"
[mcp_servers.switchboard]
command = '/Applications/My "Tools"/bridge'
"#,
        );

        let args = overrides(&[user]);
        let value = args[1]
            .strip_prefix("mcp_servers.switchboard.command=")
            .unwrap();
        let parsed: toml::Table = toml::from_str(&format!("v = {value}")).unwrap();
        assert_eq!(
            parsed["v"].as_str(),
            Some(r#"/Applications/My "Tools"/bridge"#)
        );
    }

    #[test]
    fn no_matching_entry_and_missing_files_give_no_overrides() {
        let dir = TempDir::new().unwrap();
        let user = write(
            &dir.path().join("config.toml"),
            "[mcp_servers.other]\ncommand = \"other\"\n",
        );

        assert!(overrides(&[user, dir.path().join("absent.toml")]).is_empty());
    }

    #[test]
    fn an_unparseable_file_turns_off_every_override() {
        let dir = TempDir::new().unwrap();
        let system = write(
            &dir.path().join("system.toml"),
            "[mcp_servers.switchboard]\nurl = \"http://127.0.0.1:47821/mcp\"\n",
        );
        let user = write(&dir.path().join("user.toml"), "[mcp_servers.switchboard\n");

        assert!(overrides(&[system, user]).is_empty());
    }

    #[test]
    fn a_non_table_mcp_servers_turns_off_every_override() {
        let dir = TempDir::new().unwrap();
        let system = write(&dir.path().join("system.toml"), "mcp_servers = 3\n");
        let user = write(
            &dir.path().join("user.toml"),
            "[mcp_servers.switchboard]\nurl = \"http://127.0.0.1:47821/mcp\"\n",
        );

        assert!(overrides(&[system, user]).is_empty());
    }

    #[test]
    fn an_unreadable_file_turns_off_every_override() {
        let dir = TempDir::new().unwrap();
        // A directory where the file should be fails to read with an error
        // other than NotFound under any user, root included, unlike a mode bit.
        let system = dir.path().join("system.toml");
        std::fs::create_dir(&system).unwrap();
        let user = write(
            &dir.path().join("user.toml"),
            "[mcp_servers.switchboard]\nurl = \"http://127.0.0.1:47821/mcp\"\n",
        );

        assert!(overrides(&[system, user]).is_empty());
    }

    #[test]
    fn different_transports_in_two_files_skip_that_entry_only() {
        let dir = TempDir::new().unwrap();
        let system = write(
            &dir.path().join("system.toml"),
            "[mcp_servers.switchboard]\ncommand = \"npx\"\n",
        );
        let user = write(
            &dir.path().join("user.toml"),
            r#"
[mcp_servers.switchboard]
url = "http://127.0.0.1:47821/mcp"

[mcp_servers.sb_http]
url = "http://127.0.0.1:47821/mcp"
"#,
        );

        assert_eq!(
            overrides(&[system, user]),
            vec![
                "-c",
                r#"mcp_servers.sb_http.url="http://127.0.0.1:47821/mcp""#,
                "-c",
                "mcp_servers.sb_http.enabled=false",
            ]
        );
    }

    #[test]
    fn same_transport_in_two_files_uses_the_user_files_value() {
        let dir = TempDir::new().unwrap();
        let system = write(
            &dir.path().join("system.toml"),
            "[mcp_servers.switchboard]\nurl = \"http://127.0.0.1:47821/mcp\"\n",
        );
        let user = write(
            &dir.path().join("user.toml"),
            "[mcp_servers.switchboard]\nurl = \"http://localhost:47821/mcp\"\n",
        );

        assert_eq!(
            overrides(&[system, user])[1],
            r#"mcp_servers.switchboard.url="http://localhost:47821/mcp""#
        );
    }

    #[test]
    fn the_user_file_can_point_a_renamed_entry_away_from_this_instance() {
        let dir = TempDir::new().unwrap();
        let system = write(
            &dir.path().join("system.toml"),
            "[mcp_servers.sb]\nurl = \"http://127.0.0.1:47821/mcp\"\n",
        );
        let user = write(
            &dir.path().join("user.toml"),
            "[mcp_servers.sb]\nurl = \"http://127.0.0.1:9000/mcp\"\n",
        );

        assert!(overrides(&[system, user]).is_empty());
    }

    #[test]
    fn unknown_shapes_skip_that_entry_only() {
        let dir = TempDir::new().unwrap();
        for shape in [
            "url = \"http://127.0.0.1:47821/mcp\"\ncommand = \"npx\"",
            "enabled = true",
            "url = 47821",
            "command = [\"npx\"]",
        ] {
            let user = write(
                &dir.path().join("user.toml"),
                &format!(
                    "[mcp_servers.switchboard]\n{shape}\n\n\
                     [mcp_servers.sb_http]\nurl = \"http://127.0.0.1:47821/mcp\"\n"
                ),
            );

            let args = overrides(&[user]);
            assert_eq!(
                args,
                vec![
                    "-c",
                    r#"mcp_servers.sb_http.url="http://127.0.0.1:47821/mcp""#,
                    "-c",
                    "mcp_servers.sb_http.enabled=false",
                ],
                "{shape}"
            );
        }
    }

    #[test]
    fn a_non_table_entry_named_switchboard_is_skipped() {
        let dir = TempDir::new().unwrap();
        let user = write(
            &dir.path().join("user.toml"),
            "[mcp_servers]\nswitchboard = 1\n",
        );

        assert!(overrides(&[user]).is_empty());
    }

    #[test]
    fn a_name_a_key_path_cannot_carry_is_skipped() {
        let dir = TempDir::new().unwrap();
        let user = write(
            &dir.path().join("user.toml"),
            "[mcp_servers.\"sb.http\"]\nurl = \"http://127.0.0.1:47821/mcp\"\n",
        );

        assert!(overrides(&[user]).is_empty());
    }

    #[test]
    fn a_required_entry_is_turned_off_too() {
        // Codex requires a server only while it is enabled, so `enabled=false`
        // can't make a required entry fail the run.
        let dir = TempDir::new().unwrap();
        let user = write(
            &dir.path().join("config.toml"),
            "[mcp_servers.switchboard]\nurl = \"http://127.0.0.1:47821/mcp\"\nrequired = true\n",
        );

        assert_eq!(
            overrides(&[user]),
            vec![
                "-c",
                r#"mcp_servers.switchboard.url="http://127.0.0.1:47821/mcp""#,
                "-c",
                "mcp_servers.switchboard.enabled=false",
            ]
        );
    }

    #[test]
    fn codex_home_defaults_to_dot_codex_under_home() {
        let home = Path::new("/Users/someone");
        let cwd = Path::new("/work/project");

        assert_eq!(codex_home(None, home, cwd), home.join(".codex"));
        assert_eq!(
            codex_home(Some(OsStr::new("")), home, cwd),
            home.join(".codex")
        );
    }

    #[test]
    fn codex_home_resolves_a_relative_value_from_the_agents_folder() {
        let home = Path::new("/Users/someone");
        let cwd = Path::new("/work/project");

        assert_eq!(
            codex_home(Some(OsStr::new("codex-state")), home, cwd),
            Path::new("/work/project/codex-state")
        );
        assert_eq!(
            codex_home(Some(OsStr::new("/opt/codex")), home, cwd),
            Path::new("/opt/codex")
        );
    }

    /// A loopback listener on a free port that counts the connections it
    /// accepts and closes each at once.
    async fn counting_listener() -> (u16, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let count = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&count);
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                seen.fetch_add(1, Ordering::SeqCst);
                drop(stream);
            }
        });
        (port, count, task)
    }

    /// One `codex exec` turn with the adapter's own first-turn argv and
    /// `overrides`: whether it completed, and its output for the report.
    async fn run_codex(cwd: &Path, overrides: &[String]) -> (bool, String) {
        let agent = AgentRecord {
            id: uuid::Uuid::now_v7(),
            project_id: uuid::Uuid::now_v7(),
            name: "live".to_owned(),
            harness: HarnessKind::Codex,
            session_locator: None,
            model: None,
            effort: Some("low".to_owned()),
            model_choices: Vec::new(),
            effort_choices: Vec::new(),
            forked_from_session: None,
            created_at: chrono::Utc::now(),
        };
        let args =
            super::super::build_args(&agent, "Reply with only the word ack.", None, overrides);
        let output = tokio::time::timeout(
            Duration::from_mins(3),
            tokio::process::Command::new("codex")
                .args(&args)
                .current_dir(cwd)
                .env(TOKEN_VARIABLE, "switchboard-live-test-dummy")
                .stdin(Stdio::null())
                .kill_on_drop(true)
                .output(),
        )
        .await
        .expect("codex finished within three minutes")
        .expect("codex started");
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        let completed = output.status.success() && stdout.contains("\"turn.completed\"");
        (completed, format!("{}\n{stdout}\n{stderr}", output.status))
    }

    /// The variable the setup copy's Codex entry reads its token from. The
    /// test's Codex gets a dummy value, so no real token reaches a listener.
    const TOKEN_VARIABLE: &str = "SWITCHBOARD_TOKEN";

    /// The transport key of the developer's own `switchboard` entry, which a
    /// `-c` entry of that name merges into, or `None` without one.
    fn developers_switchboard_transport(cwd: &Path) -> Option<&'static str> {
        let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
        let files = CodexConfigSources::from_env().files(&home, cwd);
        let merged = merged_servers(&files).expect("your Codex config files parse");
        let entry = merged.get(OWN_MCP_SERVER_NAME)?;
        match (entry.get("url"), entry.get("command")) {
            (Some(_), None) => Some("url"),
            (None, Some(_)) => Some("command"),
            (None, None) => None,
            (Some(_), Some(_)) => {
                panic!("your `switchboard` entry has both url and command, which Codex rejects")
            }
        }
    }

    /// The overrides keep every matching server from starting: an entry named
    /// `switchboard` at another address, a renamed one at this port by
    /// `localhost`, and a stdio bridge whose argument is this port's URL. The
    /// entries come in with `-c`, so the test writes nothing in `~/.codex`.
    /// `-c` merges the named entry into the developer's own `switchboard`
    /// entry, so it takes that entry's transport (a second one fails every
    /// run), sets `enabled` and `required` so the developer's values can't
    /// break the control run, and carries the setup copy's keys. The last run
    /// passes the overrides alone, so the renamed entries are missing from what
    /// Codex loaded, as they would be if Switchboard read a file Codex didn't:
    /// the repeated transport still lets the run start.
    #[tokio::test]
    #[ignore = "requires codex installed — run with: make test-live"]
    async fn live_codex_own_server_overrides_keep_matching_servers_from_starting() {
        let (own_port, own_hits, own_task) = counting_listener().await;
        let (named_port, named_hits, named_task) = counting_listener().await;
        let dir = TempDir::new().unwrap();
        let cwd = dir.path().join("work");
        std::fs::create_dir(&cwd).unwrap();
        let named_marker = dir.path().join("named-started");
        let bridge_marker = dir.path().join("bridge-started");
        let named_is_stdio = developers_switchboard_transport(&cwd) == Some("command");
        let mut entries = if named_is_stdio {
            vec![
                r#"mcp_servers.switchboard.command="sh""#.to_owned(),
                format!(
                    r#"mcp_servers.switchboard.args=["-c", "touch '{}'"]"#,
                    named_marker.display()
                ),
            ]
        } else {
            vec![
                format!(r#"mcp_servers.switchboard.url="http://127.0.0.1:{named_port}/mcp""#),
                format!(r#"mcp_servers.switchboard.bearer_token_env_var="{TOKEN_VARIABLE}""#),
            ]
        };
        entries.extend([
            r#"mcp_servers.switchboard.default_tools_approval_mode="approve""#.to_owned(),
            "mcp_servers.switchboard.enabled=true".to_owned(),
            "mcp_servers.switchboard.required=false".to_owned(),
            format!(r#"mcp_servers.sb_localhost.url="http://localhost:{own_port}/mcp""#),
            r#"mcp_servers.sb_bridge.command="sh""#.to_owned(),
            format!(
                r#"mcp_servers.sb_bridge.args=["-c", "touch '{}'", "http://127.0.0.1:{own_port}/mcp"]"#,
                bridge_marker.display()
            ),
        ]);
        let named_started = || {
            if named_is_stdio {
                named_marker.exists()
            } else {
                named_hits.load(Ordering::SeqCst) > 0
            }
        };
        let fixture = write(
            &dir.path().join("config.toml"),
            &format!("{}\n", entries.join("\n")),
        );
        let injected: Vec<String> = entries
            .iter()
            .flat_map(|pair| ["-c".to_owned(), pair.clone()])
            .collect();
        let overrides = overrides_from_files(&[fixture], OwnMcpServer::new(own_port));
        assert_eq!(overrides.len(), 12, "three entries matched: {overrides:?}");

        let (completed, log) = run_codex(&cwd, &injected).await;
        assert!(completed, "the control run completed: {log}");
        assert!(
            named_started(),
            "control started the entry named switchboard"
        );
        assert!(
            own_hits.load(Ordering::SeqCst) > 0,
            "control reached localhost"
        );
        assert!(bridge_marker.exists(), "control started the stdio bridge");

        named_hits.store(0, Ordering::SeqCst);
        own_hits.store(0, Ordering::SeqCst);
        std::fs::remove_file(&bridge_marker).unwrap();
        if named_is_stdio {
            std::fs::remove_file(&named_marker).unwrap();
        }
        let (completed, log) = run_codex(&cwd, &[injected, overrides.clone()].concat()).await;
        assert!(completed, "the blocked run completed: {log}");
        assert!(!named_started(), "by name");
        assert_eq!(own_hits.load(Ordering::SeqCst), 0, "by address");
        assert!(!bridge_marker.exists(), "by a stdio argument");

        let (completed, log) = run_codex(&cwd, &overrides).await;
        assert!(completed, "the overrides alone still start: {log}");
        assert!(!named_started());
        assert_eq!(own_hits.load(Ordering::SeqCst), 0);
        assert!(!bridge_marker.exists());

        own_task.abort();
        named_task.abort();
    }
}
