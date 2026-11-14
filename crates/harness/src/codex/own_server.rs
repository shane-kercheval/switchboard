//! Finds Switchboard's own MCP server in Codex's config files and builds the
//! `-c` overrides that turn it off for one run (see [`crate::own_mcp`]).
//!
//! Codex has no per-run deny list, so each matching entry is switched off with
//! `-c mcp_servers.<name>.enabled=false`. That override alone is unsafe:
//! `-c` merges into the user's table, and for a name Codex didn't load (an
//! untrusted folder's file, say) it leaves an entry with no transport, which
//! fails the run with "invalid transport in `mcp_servers.<name>`". So each
//! override also repeats the entry's own transport key with its own value
//! (`url` or `command`), which is valid whether or not Codex loaded the entry.
//! A different transport key is not safe either: `-c url=…` over a stdio entry
//! fails with "url is not supported for stdio". Probed @ codex-cli 0.165.0.
//!
//! The block must never stop an agent from starting, so where an override
//! might not be valid this passes none for the entry and logs a warning:
//! - a config file that can't be read or parsed (no overrides at all, since
//!   the file could redefine any entry);
//! - an entry with both `url` and `command` after merging the files, which
//!   includes the same name with a different transport in two files;
//! - an entry with neither, a non-string transport, or one that isn't a table;
//! - an entry with `required = true`, untested with `enabled = false`;
//! - a name outside `[A-Za-z0-9_-]`, which can't be written as a `-c` key path.
//!
//! Files read, lowest precedence first: `$CODEX_HOME/config.toml` (default
//! `~/.codex/config.toml`), then `.codex/config.toml` in each folder from the
//! git root (the first ancestor holding `.git`; with none, the filesystem root)
//! down to the working folder. Codex reads project files only in trusted
//! folders; reading them regardless adds at most a disabled entry Codex never
//! had. `/etc/codex/config.toml` and managed config are not read.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::own_mcp::{OWN_MCP_SERVER_NAME, OwnMcpServer};

/// The `-c` arguments that turn Switchboard's own server off for a Codex run
/// in `cwd`, or none when `own` is `None`.
pub(crate) fn own_server_overrides(
    own: Option<OwnMcpServer>,
    home: &Path,
    cwd: &Path,
) -> Vec<String> {
    let Some(own) = own else {
        return Vec::new();
    };
    overrides_from_layers(&config_layers(&codex_home(home), cwd), own)
}

/// `$CODEX_HOME` when set, as the Codex child inherits it; else `<home>/.codex`.
fn codex_home(home: &Path) -> PathBuf {
    std::env::var_os("CODEX_HOME")
        .filter(|value| !value.is_empty())
        .map_or_else(|| home.join(".codex"), PathBuf::from)
}

/// The config files Codex may read for a run in `cwd`, lowest precedence first.
fn config_layers(codex_home: &Path, cwd: &Path) -> Vec<PathBuf> {
    let user = codex_home.join("config.toml");
    let mut project: Vec<PathBuf> = Vec::new();
    for dir in cwd.ancestors() {
        project.push(dir.join(".codex").join("config.toml"));
        if dir.join(".git").exists() {
            break;
        }
    }
    project.reverse();
    let mut layers = vec![user];
    for path in project {
        if !layers.iter().any(|seen| same_file(seen, &path)) {
            layers.push(path);
        }
    }
    layers
}

fn same_file(a: &Path, b: &Path) -> bool {
    a == b
        || matches!(
            (a.canonicalize(), b.canonicalize()),
            (Ok(a), Ok(b)) if a == b
        )
}

fn overrides_from_layers(layers: &[PathBuf], own: OwnMcpServer) -> Vec<String> {
    let mut merged: BTreeMap<String, toml::Value> = BTreeMap::new();
    for path in layers {
        let Some(servers) = read_servers(path) else {
            tracing::warn!(
                path = %path.display(),
                "Codex config unreadable; not turning off Switchboard's own MCP server for this run"
            );
            return Vec::new();
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
    if table.get("required").and_then(toml::Value::as_bool) == Some(true) {
        return Err("entry is required = true");
    }
    match (table.get("url"), table.get("command")) {
        (Some(toml::Value::String(url)), None) => Ok(("url", url)),
        (None, Some(toml::Value::String(command))) => Ok(("command", command)),
        (Some(_), Some(_)) => Err("entry has both url and command, possibly from two files"),
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

    fn overrides(layers: &[PathBuf]) -> Vec<String> {
        overrides_from_layers(layers, OWN)
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
        let user = write(
            &dir.path().join("user.toml"),
            "[mcp_servers.switchboard]\nurl = \"http://127.0.0.1:47821/mcp\"\n",
        );
        let project = write(
            &dir.path().join("project.toml"),
            "[mcp_servers.switchboard\n",
        );

        assert!(overrides(&[user, project]).is_empty());
    }

    #[test]
    fn a_non_table_mcp_servers_turns_off_every_override() {
        let dir = TempDir::new().unwrap();
        let user = write(
            &dir.path().join("user.toml"),
            "[mcp_servers.switchboard]\nurl = \"http://127.0.0.1:47821/mcp\"\n",
        );
        let project = write(&dir.path().join("project.toml"), "mcp_servers = 3\n");

        assert!(overrides(&[user, project]).is_empty());
    }

    #[test]
    fn an_unreadable_file_turns_off_every_override() {
        let dir = TempDir::new().unwrap();
        let user = write(
            &dir.path().join("user.toml"),
            "[mcp_servers.switchboard]\nurl = \"http://127.0.0.1:47821/mcp\"\n",
        );
        // A directory where the file should be fails to read with an error
        // other than NotFound under any user, root included, unlike a mode bit.
        let project = dir.path().join("project.toml");
        std::fs::create_dir(&project).unwrap();

        assert!(overrides(&[user, project]).is_empty());
    }

    #[test]
    fn different_transports_in_two_files_skip_that_entry_only() {
        let dir = TempDir::new().unwrap();
        let user = write(
            &dir.path().join("user.toml"),
            r#"
[mcp_servers.switchboard]
url = "http://127.0.0.1:47821/mcp"

[mcp_servers.sb_http]
url = "http://127.0.0.1:47821/mcp"
"#,
        );
        let project = write(
            &dir.path().join("project.toml"),
            "[mcp_servers.switchboard]\ncommand = \"npx\"\n",
        );

        assert_eq!(
            overrides(&[user, project]),
            vec![
                "-c",
                r#"mcp_servers.sb_http.url="http://127.0.0.1:47821/mcp""#,
                "-c",
                "mcp_servers.sb_http.enabled=false",
            ]
        );
    }

    #[test]
    fn same_transport_in_two_files_uses_the_higher_precedence_value() {
        let dir = TempDir::new().unwrap();
        let user = write(
            &dir.path().join("user.toml"),
            "[mcp_servers.switchboard]\nurl = \"http://127.0.0.1:47821/mcp\"\n",
        );
        let project = write(
            &dir.path().join("project.toml"),
            "[mcp_servers.switchboard]\nurl = \"http://localhost:47821/mcp\"\n",
        );

        assert_eq!(
            overrides(&[user, project])[1],
            r#"mcp_servers.switchboard.url="http://localhost:47821/mcp""#
        );
    }

    #[test]
    fn a_later_file_can_point_a_renamed_entry_away_from_this_instance() {
        let dir = TempDir::new().unwrap();
        let user = write(
            &dir.path().join("user.toml"),
            "[mcp_servers.sb]\nurl = \"http://127.0.0.1:47821/mcp\"\n",
        );
        let project = write(
            &dir.path().join("project.toml"),
            "[mcp_servers.sb]\nurl = \"http://127.0.0.1:9000/mcp\"\n",
        );

        assert!(overrides(&[user, project]).is_empty());
    }

    #[test]
    fn unknown_shapes_skip_that_entry_only() {
        let dir = TempDir::new().unwrap();
        for shape in [
            "url = \"http://127.0.0.1:47821/mcp\"\ncommand = \"npx\"",
            "enabled = true",
            "url = 47821",
            "command = [\"npx\"]",
            "url = \"http://127.0.0.1:47821/mcp\"\nrequired = true",
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
    fn layers_run_from_the_user_file_through_the_git_root_down_to_the_cwd() {
        let root = TempDir::new().unwrap();
        let repo = root.path().join("repo");
        let cwd = repo.join("a").join("b");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir(repo.join(".git")).unwrap();
        let codex_home = root.path().join("codex-home");

        assert_eq!(
            config_layers(&codex_home, &cwd),
            vec![
                codex_home.join("config.toml"),
                repo.join(".codex/config.toml"),
                repo.join("a/.codex/config.toml"),
                repo.join("a/b/.codex/config.toml"),
            ]
        );
    }

    #[test]
    fn layers_without_a_git_root_reach_the_filesystem_root() {
        let root = TempDir::new().unwrap();
        let codex_home = root.path().join("codex-home");

        let layers = config_layers(&codex_home, root.path());
        assert_eq!(layers[0], codex_home.join("config.toml"));
        assert_eq!(layers[1], Path::new("/.codex/config.toml"));
        assert_eq!(
            layers.last().unwrap(),
            &root.path().join(".codex/config.toml")
        );
    }

    #[test]
    fn the_user_file_is_not_read_twice_when_the_cwd_is_under_home() {
        let home = TempDir::new().unwrap();
        let cwd = home.path().join("project");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir(home.path().join(".git")).unwrap();
        let codex_home = home.path().join(".codex");

        assert_eq!(
            config_layers(&codex_home, &cwd),
            vec![
                codex_home.join("config.toml"),
                cwd.join(".codex/config.toml"),
            ]
        );
    }

    #[test]
    fn project_files_are_found_through_the_layers() {
        let root = TempDir::new().unwrap();
        let repo = root.path().join("repo");
        let cwd = repo.join("sub");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir(repo.join(".git")).unwrap();
        write(
            &repo.join(".codex/config.toml"),
            "[mcp_servers.switchboard]\nurl = \"http://127.0.0.1:47821/mcp\"\n",
        );

        let layers = config_layers(&root.path().join("codex-home"), &cwd);
        assert_eq!(overrides(&layers).len(), 4);
    }

    #[test]
    fn no_own_server_reads_nothing_and_passes_nothing() {
        let dir = TempDir::new().unwrap();
        write(
            &dir.path().join(".codex/config.toml"),
            "[mcp_servers.switchboard]\nurl = \"http://127.0.0.1:47821/mcp\"\n",
        );

        assert!(own_server_overrides(None, dir.path(), dir.path()).is_empty());
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

    /// The finder's overrides, laid over entries passed with `-c` in place of
    /// the user's config file (so the test reads and writes nothing in
    /// `~/.codex`), keep every matching server from starting: an HTTP entry
    /// named `switchboard` at another port, a renamed one at this port by
    /// `localhost`, and a stdio bridge whose argument is this port's URL. The
    /// overrides alone, as for an entry Codex never loaded (an untrusted
    /// folder's file), still let the run start.
    #[tokio::test]
    #[ignore = "requires codex installed — run with: make test-live"]
    async fn live_codex_own_server_overrides_keep_matching_servers_from_starting() {
        let (own_port, own_hits, own_task) = counting_listener().await;
        let (named_port, named_hits, named_task) = counting_listener().await;
        let dir = TempDir::new().unwrap();
        let cwd = dir.path().join("work");
        std::fs::create_dir(&cwd).unwrap();
        let marker = dir.path().join("bridge-started");
        let entries = [
            format!(r#"mcp_servers.switchboard.url="http://127.0.0.1:{named_port}/mcp""#),
            format!(r#"mcp_servers.sb_localhost.url="http://localhost:{own_port}/mcp""#),
            r#"mcp_servers.sb_bridge.command="sh""#.to_owned(),
            format!(
                r#"mcp_servers.sb_bridge.args=["-c", "touch '{}'", "http://127.0.0.1:{own_port}/mcp"]"#,
                marker.display()
            ),
        ];
        let fixture = write(
            &dir.path().join("config.toml"),
            &format!("{}\n", entries.join("\n")),
        );
        let injected: Vec<String> = entries
            .iter()
            .flat_map(|pair| ["-c".to_owned(), pair.clone()])
            .collect();
        let overrides = overrides_from_layers(&[fixture], OwnMcpServer::new(own_port));
        assert_eq!(overrides.len(), 12, "three entries matched: {overrides:?}");

        let (completed, log) = run_codex(&cwd, &injected).await;
        assert!(completed, "the control run completed: {log}");
        assert!(
            named_hits.load(Ordering::SeqCst) > 0,
            "control reached the named entry"
        );
        assert!(
            own_hits.load(Ordering::SeqCst) > 0,
            "control reached localhost"
        );
        assert!(marker.exists(), "control started the stdio bridge");

        named_hits.store(0, Ordering::SeqCst);
        own_hits.store(0, Ordering::SeqCst);
        std::fs::remove_file(&marker).unwrap();
        let (completed, log) = run_codex(&cwd, &[injected, overrides.clone()].concat()).await;
        assert!(completed, "the blocked run completed: {log}");
        assert_eq!(named_hits.load(Ordering::SeqCst), 0, "by name");
        assert_eq!(own_hits.load(Ordering::SeqCst), 0, "by address");
        assert!(!marker.exists(), "by a stdio argument");

        let (completed, log) = run_codex(&cwd, &overrides).await;
        assert!(completed, "the overrides alone still start: {log}");
        assert_eq!(named_hits.load(Ordering::SeqCst), 0);
        assert_eq!(own_hits.load(Ordering::SeqCst), 0);
        assert!(!marker.exists());

        own_task.abort();
        named_task.abort();
    }
}
