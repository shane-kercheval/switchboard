//! Ending the commands `agy` runs on terminals of their own when Switchboard
//! kills `agy`.
//!
//! `agy` runs each `run_command` call as `zsh -c <command>`, leading its own
//! session on a terminal `agy` opens, outside the process group the adapter
//! signals, and with SIGHUP ignored or handled. Once `agy` exits the shell
//! belongs to launchd and runs on, so killing `agy` alone would leave a long
//! build or test changing files (G52 in `docs/harness-behavior.md`). The
//! shells can be found only while `agy` is alive, by walking its process tree,
//! so they and every process under them are recorded then, and the process
//! groups of those still running are signalled once `agy` is gone.
//!
//! A command shell is a descendant of `agy` that leads its own session and has
//! a terminal. That leaves out `agy --bg-updater`, the auto-updater, which
//! leads a session with no terminal and is meant to outlive `agy`, and MCP
//! servers, which run in `agy`'s own group. The walk does not enter a process
//! being debugged: macOS reports a debugged process's debugger as its parent.
//!
//! Processes are read from `/bin/ps`: calling libproc directly needs the
//! `unsafe` the workspace forbids, and the `libproc` crate needs libclang to
//! build. A listing that fails or does not parse is skipped, so nothing is
//! signalled, and the kill is the group kill alone, when no listing before
//! `agy` exits records a shell or when the listing after it exits fails.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::process::Stdio;
use std::time::Duration;

use nix::sys::signal::{Signal, killpg};
use nix::unistd::{Pid, getpgrp};

use crate::events::TurnId;
use crate::subprocess::TERMINATE_GRACE;

/// How long one `ps` call may take before the snapshot is given up.
const PS_LIMIT: Duration = Duration::from_secs(2);

/// The pause between snapshots while `agy` exits, and between checks on
/// whether the shells' groups have emptied.
const POLL: Duration = Duration::from_millis(20);

/// One row of `ps` output.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Process {
    pid: u32,
    ppid: u32,
    pgid: u32,
    session_leader: bool,
    has_terminal: bool,
    traced: bool,
    /// `ps`'s start time, compared only for equality: the same pid with a
    /// different start time is another process.
    started: String,
}

/// A process seen in one snapshot, to be found again in a later one.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Recorded {
    pid: u32,
    started: String,
    /// The group it was in when seen.
    pgid: u32,
}

impl Recorded {
    /// Whether `process` is this process: the same pid with another start
    /// time is another process.
    fn is(&self, process: &Process) -> bool {
        self.pid == process.pid && self.started == process.started
    }
}

impl From<&Process> for Recorded {
    fn from(process: &Process) -> Self {
        Self {
            pid: process.pid,
            started: process.started.clone(),
            pgid: process.pgid,
        }
    }
}

/// The command shells seen under one `agy`, and every process seen under
/// them.
#[derive(Debug)]
pub(super) struct CommandShells {
    turn_id: TurnId,
    recorded: Vec<Recorded>,
    warned: bool,
}

impl CommandShells {
    pub(super) fn new(turn_id: TurnId) -> Self {
        Self {
            turn_id,
            recorded: Vec::new(),
            warned: false,
        }
    }

    /// Add the command shells now under `agy` and every process now under a
    /// recorded process.
    pub(super) async fn record(&mut self, agy: u32) {
        if let Some(processes) = self.snapshot().await {
            self.note(&processes, agy);
        }
    }

    fn note(&mut self, processes: &[Process], agy: u32) {
        let shells = shells_under(processes, agy);
        self.add(shells);
        let under = still_under(processes, &self.recorded);
        self.add(under);
    }

    fn add(&mut self, found: Vec<Recorded>) {
        for found in found {
            if !self
                .recorded
                .iter()
                .any(|known| known.pid == found.pid && known.started == found.started)
            {
                self.recorded.push(found);
            }
        }
    }

    /// [`Self::record`] again and again, until the caller drops this future:
    /// a shell `agy` starts while it exits is caught up to its last moment.
    pub(super) async fn keep_recording(&mut self, agy: u32) {
        loop {
            tokio::time::sleep(POLL).await;
            self.record(agy).await;
        }
    }

    /// End the recorded processes still running and everything under them,
    /// once `agy` has been reaped: SIGTERM each process group they are in,
    /// wait up to [`TERMINATE_GRACE`] for those groups to empty, then SIGKILL
    /// a group that a new listing shows still holds one of them.
    pub(super) async fn end(mut self, agy: u32) {
        if self.recorded.is_empty() {
            return;
        }
        let Some(now) = self.snapshot().await else {
            return;
        };
        let members = still_under(&now, &self.recorded);
        let groups = groups_of(&members, agy, getpgrp().as_raw().cast_unsigned());
        if groups.is_empty() {
            return;
        }
        tracing::info!(
            turn_id = %self.turn_id, groups = ?groups,
            "antigravity: ending the commands agy ran on terminals of their own"
        );
        for &group in &groups {
            let _ = killpg(as_pid(group), Signal::SIGTERM);
        }
        let grace_ends = tokio::time::Instant::now() + TERMINATE_GRACE;
        let mut remaining = groups;
        loop {
            remaining.retain(|&group| killpg(as_pid(group), None).is_ok());
            if remaining.is_empty() || tokio::time::Instant::now() >= grace_ends {
                break;
            }
            tokio::time::sleep(POLL).await;
        }
        if remaining.is_empty() {
            return;
        }
        let Some(later) = self.snapshot().await else {
            return;
        };
        for group in still_held(&later, &members, &remaining) {
            let _ = killpg(as_pid(group), Signal::SIGKILL);
        }
    }

    /// Every process now running, or `None` after logging why not; the
    /// warning is logged once per stop.
    async fn snapshot(&mut self) -> Option<Vec<Process>> {
        match read_processes().await {
            Ok(processes) => Some(processes),
            Err(reason) => {
                if !self.warned {
                    self.warned = true;
                    tracing::warn!(
                        turn_id = %self.turn_id, %reason,
                        "antigravity: could not list processes, so commands agy runs on \
                         terminals of their own are left running"
                    );
                }
                None
            }
        }
    }
}

fn as_pid(id: u32) -> Pid {
    Pid::from_raw(id.cast_signed())
}

async fn read_processes() -> Result<Vec<Process>, String> {
    let output = tokio::time::timeout(
        PS_LIMIT,
        tokio::process::Command::new("/bin/ps")
            .args(["-A", "-o", "pid=,ppid=,pgid=,stat=,tt=,lstart="])
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| format!("ps took longer than {PS_LIMIT:?}"))?
    .map_err(|e| format!("cannot run ps: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "ps exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let text = String::from_utf8(output.stdout).map_err(|_| "ps wrote non-UTF-8".to_owned())?;
    parse_processes(&text)
}

/// Every row of `ps -o pid=,ppid=,pgid=,stat=,tt=,lstart=`. One row that does
/// not parse fails the whole snapshot: a tree with a row missing could hide a
/// process's real parent.
fn parse_processes(text: &str) -> Result<Vec<Process>, String> {
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| parse_process(line).ok_or_else(|| format!("unexpected ps line {line:?}")))
        .collect()
}

fn parse_process(line: &str) -> Option<Process> {
    let (process, rest) = next_field(line)?;
    let (parent, rest) = next_field(rest)?;
    let (group, rest) = next_field(rest)?;
    let (stat, rest) = next_field(rest)?;
    let (tt, rest) = next_field(rest)?;
    let started = rest.trim();
    if started.is_empty() {
        return None;
    }
    Some(Process {
        pid: process.parse().ok()?,
        ppid: parent.parse().ok()?,
        pgid: group.parse().ok()?,
        // `ps` marks a session leader with a lowercase `s` among its state
        // flags and a process being debugged with `X`.
        session_leader: stat.contains('s'),
        has_terminal: is_pseudo_terminal(tt),
        traced: stat.contains('X'),
        started: started.to_owned(),
    })
}

/// Whether `ps` names a pseudo-terminal, as `agy` opens for a command: `s`
/// and digits, from `/dev/ttys001`. Anything else counts as no terminal,
/// including `??` for none and a trailing `-` for a revoked one, so a change
/// in how `ps` writes the column spares a process rather than ending it.
fn is_pseudo_terminal(tt: &str) -> bool {
    tt.strip_prefix('s')
        .is_some_and(|number| !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()))
}

/// The first whitespace-separated field of `text`, and what follows it.
fn next_field(text: &str) -> Option<(&str, &str)> {
    let text = text.trim_start();
    let end = text.find(char::is_whitespace)?;
    Some((&text[..end], &text[end..]))
}

/// Every process under `root`, not counting `root`. The walk does not enter a
/// process being debugged: macOS reports its debugger as its parent, so it
/// may have been started anywhere.
fn descendants(processes: &[Process], root: u32) -> Vec<&Process> {
    let mut children: HashMap<u32, Vec<&Process>> = HashMap::new();
    for process in processes {
        if process.pid != process.ppid && !process.traced {
            children.entry(process.ppid).or_default().push(process);
        }
    }
    let mut found = Vec::new();
    let mut seen = HashSet::from([root]);
    let mut stack = vec![root];
    while let Some(parent) = stack.pop() {
        for &child in children.get(&parent).into_iter().flatten() {
            if seen.insert(child.pid) {
                found.push(child);
                stack.push(child.pid);
            }
        }
    }
    found
}

/// The command shells under `agy`: descendants that lead their own session
/// and have a terminal.
fn shells_under(processes: &[Process], agy: u32) -> Vec<Recorded> {
    descendants(processes, agy)
        .into_iter()
        .filter(|process| {
            process.session_leader && process.has_terminal && process.pgid == process.pid
        })
        .map(Recorded::from)
        .collect()
}

/// The recorded processes still running in `processes`, each found by its
/// pid and start time, and every process under them, with the groups they
/// are in now.
fn still_under(processes: &[Process], recorded: &[Recorded]) -> Vec<Recorded> {
    let mut members = Vec::new();
    for known in recorded {
        let Some(running) = processes.iter().find(|process| known.is(process)) else {
            continue;
        };
        for process in std::iter::once(running).chain(descendants(processes, running.pid)) {
            let member = Recorded::from(process);
            if !members.contains(&member) {
                members.push(member);
            }
        }
    }
    members
}

/// The process groups `members` are in. Each member was found under a shell
/// through parents not being debugged, and a shell leads its own session, in
/// which every process was forked from one already in it, so each of these
/// groups holds only processes started under a shell. Groups 0 and 1, `agy`'s
/// group and Switchboard's own group (`own`) are left out all the same.
fn groups_of(members: &[Recorded], agy: u32, own: u32) -> BTreeSet<u32> {
    members
        .iter()
        .map(|member| member.pgid)
        .filter(|&group| group > 1 && group != agy && group != own)
        .collect()
}

/// The groups in `groups` that still hold one of `members`: a process in
/// `processes` with a member's pid, start time and group. A group holding one
/// is still the group recorded, whatever else has joined it.
fn still_held(
    processes: &[Process],
    members: &[Recorded],
    groups: &BTreeSet<u32>,
) -> BTreeSet<u32> {
    processes
        .iter()
        .filter(|process| groups.contains(&process.pgid))
        .filter(|process| members.contains(&Recorded::from(*process)))
        .map(|process| process.pgid)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ps -A -o pid=,ppid=,pgid=,stat=,tt=,lstart=` under `LC_ALL=C`, run
    /// while agy 1.5.13 (8970, started by a probe, 8966) waited on a command
    /// it had moved to the background: its shell (9022) and the shell's
    /// `sleep` (9024). The updater (8973) is from the same run, read a few
    /// seconds earlier before it exited; the MCP server (8980) is a stdio
    /// server from another run, placed under this `agy`; 501 is a terminal
    /// shell outside `agy`'s tree.
    const PROBE: &str = concat!(
        " 8966  8964  8964 S      ??  Thu Nov 19 23:29:12 2026    \n",
        " 8970  8966  8970 S      ??  Thu Nov 19 23:29:12 2026    \n",
        " 8973  8970  8973 Ss     ??  Thu Nov 19 23:29:12 2026    \n",
        " 8980  8970  8970 S      ??  Thu Nov 19 23:29:14 2026    \n",
        " 9022  8970  9022 Ss+  s001  Thu Nov 19 23:29:21 2026    \n",
        " 9024  9022  9022 S+   s001  Thu Nov 19 23:29:21 2026    \n",
        "  501   400   501 Ss   s002  Thu Nov 19 08:00:01 2026    \n",
    );

    const AGY: u32 = 8970;
    /// Switchboard's own process group.
    const OWN: u32 = 8964;

    fn probe() -> Vec<Process> {
        parse_processes(PROBE).unwrap()
    }

    fn pids(recorded: &[Recorded]) -> Vec<u32> {
        recorded.iter().map(|r| r.pid).collect()
    }

    #[test]
    fn a_row_parses_with_its_start_time_taken_whole() {
        assert_eq!(
            parse_process(" 9022  8970  9022 Ss+  s001  Thu Nov 19 23:29:21 2026    "),
            Some(Process {
                pid: 9022,
                ppid: 8970,
                pgid: 9022,
                session_leader: true,
                has_terminal: true,
                traced: false,
                started: "Thu Nov 19 23:29:21 2026".to_owned(),
            })
        );
        assert_eq!(
            parse_process(" 8973  8970  8973 Ss     ??  Thu Nov 19 23:29:12 2026")
                .map(|p| (p.session_leader, p.has_terminal)),
            Some((true, false))
        );
    }

    #[test]
    fn only_a_pseudo_terminal_counts_as_a_terminal() {
        let terminal = |tt: &str| {
            parse_process(&format!(
                " 9022  8970  9022 Ss+  {tt}  Thu Nov 19 23:29:21 2026"
            ))
            .map(|process| process.has_terminal)
        };
        assert_eq!(terminal("s001"), Some(true));
        assert_eq!(terminal("s1024"), Some(true));
        // Revoked, none, the console, and shapes `ps` has not been seen to write.
        for tt in ["s001-", "??", "con", "s", "sx01", "001", "-"] {
            assert_eq!(terminal(tt), Some(false), "{tt}");
        }
    }

    #[test]
    fn a_row_that_does_not_parse_fails_the_whole_snapshot() {
        assert!(parse_processes("9022 8970 9022 Ss+ s001\n").is_err());
        assert!(parse_processes("9022 8970 x Ss+ s001 Thu Nov 19 23:29:21 2026\n").is_err());
        assert!(parse_processes(&format!("{PROBE}garbage\n")).is_err());
        assert_eq!(parse_processes("\n  \n"), Ok(Vec::new()));
    }

    #[test]
    fn only_the_shell_agy_runs_a_command_in_is_a_command_shell() {
        // Not the updater (no terminal), the MCP server or `agy` (its group),
        // the shell's `sleep` (no session of its own), or a terminal shell
        // outside `agy`'s tree.
        assert_eq!(pids(&shells_under(&probe(), AGY)), vec![9022]);
    }

    #[test]
    fn a_shell_is_found_under_a_helper_too() {
        let processes = parse_processes(
            " 8970  8966  8970 S      ??  Thu Nov 19 23:29:12 2026
 9000  8970  8970 S      ??  Thu Nov 19 23:29:20 2026
 9022  9000  9022 Ss+  s001  Thu Nov 19 23:29:21 2026
",
        )
        .unwrap();
        assert_eq!(pids(&shells_under(&processes, AGY)), vec![9022]);
    }

    #[test]
    fn once_agy_is_gone_the_shell_and_what_runs_under_it_are_found_by_start_time() {
        let recorded = shells_under(&probe(), AGY);
        let reparented = parse_processes(
            " 9022     1  9022 Ss+  s001  Thu Nov 19 23:29:21 2026
 9024  9022  9022 S+   s001  Thu Nov 19 23:29:21 2026
 9030  9024  9030 S+   s001  Thu Nov 19 23:29:30 2026
",
        )
        .unwrap();
        let members = still_under(&reparented, &recorded);
        assert_eq!(pids(&members), vec![9022, 9024, 9030]);
        assert_eq!(
            groups_of(&members, AGY, OWN),
            BTreeSet::from([9022, 9030]),
            "a group a process under the shell made is ended too"
        );
    }

    #[test]
    fn a_process_being_debugged_is_not_followed() {
        // The command runs `lldb` (9030), whose `debugserver` (9031) has
        // attached to a terminal shell outside `agy`'s tree (700, running
        // 701), so `ps` lists that shell as the debugger's child.
        let processes = parse_processes(concat!(
            " 8970  8966  8970 S      ??  Thu Nov 19 23:29:12 2026\n",
            " 9022  8970  9022 Ss+  s001  Thu Nov 19 23:29:21 2026\n",
            " 9030  9022  9022 S+   s001  Thu Nov 19 23:29:22 2026\n",
            " 9031  9030  9022 S+   s001  Thu Nov 19 23:29:22 2026\n",
            "  700  9031   700 SXs+ s002  Thu Nov 19 08:00:01 2026\n",
            "  701   700   701 S+   s002  Thu Nov 19 08:00:05 2026\n",
        ))
        .unwrap();
        let shells = shells_under(&processes, AGY);
        assert_eq!(
            pids(&shells),
            vec![9022],
            "a debugged shell is no command shell"
        );
        let members = still_under(&processes, &shells);
        assert_eq!(pids(&members), vec![9022, 9030, 9031]);
        assert_eq!(groups_of(&members, AGY, OWN), BTreeSet::from([9022]));
    }

    #[test]
    fn a_process_seen_under_a_shell_is_ended_after_the_shell_exits() {
        let mut shells = CommandShells::new(uuid::Uuid::nil());
        shells.note(&probe(), AGY);
        // The shell's command finished while `agy` exited, leaving its
        // `sleep` to launchd in the shell's group, with a child of its own.
        let later = parse_processes(concat!(
            " 9024     1  9022 S      ??  Thu Nov 19 23:29:21 2026\n",
            " 9040  9024  9022 S      ??  Thu Nov 19 23:29:40 2026\n",
        ))
        .unwrap();
        shells.note(&later, AGY);
        let members = still_under(&later, &shells.recorded);
        assert_eq!(pids(&members), vec![9024, 9040]);
        assert_eq!(groups_of(&members, AGY, OWN), BTreeSet::from([9022]));
    }

    #[test]
    fn a_pid_with_another_start_time_is_another_process() {
        let recorded = shells_under(&probe(), AGY);
        let reused =
            parse_processes(" 9022     1  9022 Ss+  s003  Thu Nov 19 23:40:00 2026\n").unwrap();
        assert!(still_under(&reused, &recorded).is_empty());
    }

    #[test]
    fn groups_0_1_agys_and_switchboards_are_never_targets() {
        let members = [AGY, OWN, 0, 1, 9022].map(|pgid| Recorded {
            pid: 9100 + pgid,
            started: "Thu Nov 19 23:29:21 2026".to_owned(),
            pgid,
        });
        assert_eq!(groups_of(&members, AGY, OWN), BTreeSet::from([9022]));
    }

    #[test]
    fn a_group_is_killed_only_while_it_holds_a_recorded_process() {
        let members = still_under(&probe(), &shells_under(&probe(), AGY));
        let groups = BTreeSet::from([9022]);
        // The shell ignored SIGTERM and lives on: still the group recorded.
        let shell_left =
            parse_processes(" 9022     1  9022 Ss+  s001  Thu Nov 19 23:29:21 2026\n").unwrap();
        assert_eq!(still_held(&shell_left, &members, &groups), groups);
        // Only a process the snapshot never saw is in it: left alone.
        let stranger =
            parse_processes(" 9050     1  9022 S    ??   Thu Nov 19 23:29:40 2026\n").unwrap();
        assert!(still_held(&stranger, &members, &groups).is_empty());
        // The recorded `sleep` moved to a group of its own: not this group.
        let moved =
            parse_processes(" 9024     1  9024 S    s001 Thu Nov 19 23:29:21 2026\n").unwrap();
        assert!(still_held(&moved, &members, &groups).is_empty());
    }
}
