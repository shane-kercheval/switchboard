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
//! servers, which run in `agy`'s own group.
//!
//! macOS lists a process being debugged under its debugger, so it may never
//! have been under a command at all, and it clears the traced flag as the
//! process exits but leaves it under the debugger until the debugger reaps
//! it. So a process being debugged, exiting, or a zombie is not recorded, the
//! walk does not go below it, it never makes its group a target, and it is no
//! reason for SIGKILL. A recorded process seen being debugged is also dropped
//! from the record with every process under it.
//!
//! Processes are read from `/bin/ps`: calling libproc directly needs the
//! `unsafe` the workspace forbids, and the `libproc` crate needs libclang to
//! build. A listing that fails or does not parse is skipped. No command is
//! signalled when no listing works while `agy` runs or the one after it exits
//! fails, and none is sent SIGKILL when the one after the SIGTERM fails.

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
#[expect(
    clippy::struct_excessive_bools,
    reason = "each is a separate flag `ps` prints, not a state"
)]
struct Process {
    pid: u32,
    ppid: u32,
    pgid: u32,
    session_leader: bool,
    has_terminal: bool,
    traced: bool,
    /// Exiting or a zombie.
    exiting: bool,
    /// `ps`'s start time, compared only for equality: the same pid with a
    /// different start time is another process.
    started: String,
}

impl Process {
    /// Whether this row's parent can be taken as the process that started it:
    /// not so for one being debugged, or for one exiting or a zombie, which
    /// may still be listed under a debugger.
    fn has_own_parent(&self) -> bool {
        !self.traced && !self.exiting
    }
}

/// Processes seen in earlier listings: each one's pid, with the start time it
/// had then.
type Seen = HashMap<u32, String>;

/// Whether `process` is the one `seen` holds under its pid.
fn was_seen(seen: &Seen, process: &Process) -> bool {
    seen.get(&process.pid)
        .is_some_and(|started| *started == process.started)
}

/// One listing, with each process's children, built once per listing.
struct Tree<'a> {
    processes: &'a [Process],
    children: HashMap<u32, Vec<&'a Process>>,
}

impl<'a> Tree<'a> {
    fn new(processes: &'a [Process]) -> Self {
        let mut children: HashMap<u32, Vec<&'a Process>> = HashMap::new();
        for process in processes {
            if process.pid != process.ppid {
                children.entry(process.ppid).or_default().push(process);
            }
        }
        Self {
            processes,
            children,
        }
    }

    /// Every process under the pids in `roots`, not counting them, reached
    /// through children `enter` accepts.
    fn below(
        &self,
        roots: impl IntoIterator<Item = u32>,
        enter: impl Fn(&Process) -> bool,
    ) -> Vec<&'a Process> {
        let mut stack: Vec<u32> = roots.into_iter().collect();
        let mut visited: HashSet<u32> = stack.iter().copied().collect();
        let mut found = Vec::new();
        while let Some(parent) = stack.pop() {
            for &child in self.children.get(&parent).into_iter().flatten() {
                if enter(child) && visited.insert(child.pid) {
                    found.push(child);
                    stack.push(child.pid);
                }
            }
        }
        found
    }

    /// The command shells under `agy`: descendants, reached through processes
    /// with their own parents, that lead their own session and have a
    /// terminal.
    fn shells_under(&self, agy: u32) -> Vec<&'a Process> {
        self.below([agy], Process::has_own_parent)
            .into_iter()
            .filter(|process| {
                process.session_leader && process.has_terminal && process.pgid == process.pid
            })
            .collect()
    }

    /// The processes in this listing that `known` accepts, split into those
    /// to follow and those to drop: each one being debugged, and each one
    /// under one of those. One exiting, or a zombie, is in neither.
    fn split_known(
        &self,
        known: impl Fn(&Process) -> bool,
    ) -> (Vec<&'a Process>, Vec<&'a Process>) {
        let running: Vec<&'a Process> = self.processes.iter().filter(|p| known(p)).collect();
        let debugged: Vec<u32> = running.iter().filter(|p| p.traced).map(|p| p.pid).collect();
        let mut dropped: HashSet<u32> = debugged.iter().copied().collect();
        dropped.extend(self.below(debugged, |_| true).iter().map(|p| p.pid));
        let (mut kept, dropped): (Vec<&'a Process>, Vec<&'a Process>) =
            running.into_iter().partition(|p| !dropped.contains(&p.pid));
        kept.retain(|p| !p.exiting);
        (kept, dropped)
    }

    /// `roots` and every process under them, reached through processes with
    /// their own parents.
    fn with_everything_under(&self, mut roots: Vec<&'a Process>) -> Vec<&'a Process> {
        let under = self.below(roots.iter().map(|p| p.pid), Process::has_own_parent);
        roots.extend(under);
        roots
    }
}

/// The command shells seen under one `agy`, and every process seen under
/// them.
#[derive(Debug)]
pub(super) struct CommandShells {
    turn_id: TurnId,
    recorded: Seen,
    /// Whether any listing has worked.
    listed: bool,
    /// Whether a failed listing has been logged.
    warned: bool,
}

impl CommandShells {
    pub(super) fn new(turn_id: TurnId) -> Self {
        Self {
            turn_id,
            recorded: Seen::new(),
            listed: false,
            warned: false,
        }
    }

    /// Add the command shells now under `agy`, then [`Self::follow`].
    pub(super) async fn record(&mut self, agy: u32) {
        if let Some(processes) = self.snapshot().await {
            self.note(&processes, agy);
        }
    }

    fn note(&mut self, processes: &[Process], agy: u32) {
        let tree = Tree::new(processes);
        let shells = tree.shells_under(agy);
        self.add(&shells);
        self.follow(&tree);
    }

    /// Drop the recorded processes `tree` shows being debugged, and those
    /// under them, then record every process under the rest. Returns the
    /// recorded processes running in `tree` and every process under them.
    fn follow<'a>(&mut self, tree: &Tree<'a>) -> Vec<&'a Process> {
        let (kept, dropped) = tree.split_known(|process| was_seen(&self.recorded, process));
        for process in dropped {
            self.recorded.remove(&process.pid);
        }
        let members = tree.with_everything_under(kept);
        self.add(&members);
        members
    }

    fn add(&mut self, processes: &[&Process]) {
        for process in processes {
            self.recorded.insert(process.pid, process.started.clone());
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
        if !self.listed {
            tracing::warn!(
                turn_id = %self.turn_id,
                "antigravity: no process listing worked while agy ran, so a command it ran \
                 on a terminal of its own, if any, is left running"
            );
            return;
        }
        if self.recorded.is_empty() {
            return;
        }
        let Some(now) = self.snapshot().await else {
            tracing::warn!(
                turn_id = %self.turn_id,
                "antigravity: no process listing after agy exited, so the commands it ran on \
                 terminals of their own are left running"
            );
            return;
        };
        let members = self.follow(&Tree::new(&now));
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
            tracing::warn!(
                turn_id = %self.turn_id, groups = ?remaining,
                "antigravity: no process listing after SIGTERM, so these command groups, \
                 still running, are not sent SIGKILL"
            );
            return;
        };
        for group in still_held(&later, &members, &remaining) {
            let _ = killpg(as_pid(group), Signal::SIGKILL);
        }
    }

    /// Every process now running, or `None` after logging why not; the
    /// reason is logged once per stop.
    async fn snapshot(&mut self) -> Option<Vec<Process>> {
        match read_processes().await {
            Ok(processes) => {
                self.listed = true;
                Some(processes)
            }
            Err(reason) => {
                if !self.warned {
                    self.warned = true;
                    tracing::warn!(
                        turn_id = %self.turn_id, %reason,
                        "antigravity: could not list processes"
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
        // flags, a process being debugged with `X`, one exiting with `E`, and
        // a zombie with a leading `Z`.
        session_leader: stat.contains('s'),
        has_terminal: is_pseudo_terminal(tt),
        traced: stat.contains('X'),
        exiting: stat.starts_with('Z') || stat.contains('E'),
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

/// The process groups `members` are in. Each member was found under a shell
/// through processes with their own parents, and each is in the shell's
/// session or in one a process under the shell started, in which every
/// process was forked from one already in it, so each of these groups holds
/// only processes started under a shell. Groups 0 and 1, `agy`'s
/// group and Switchboard's own group (`own`) are left out all the same.
fn groups_of(members: &[&Process], agy: u32, own: u32) -> BTreeSet<u32> {
    members
        .iter()
        .map(|member| member.pgid)
        .filter(|&group| group > 1 && group != agy && group != own)
        .collect()
}

/// The groups in `groups` that still hold one of `members`: a process in
/// `processes` with a member's pid, start time and group, not being debugged,
/// exiting or a zombie, and not under a member being debugged. A group
/// holding one is still the group recorded, whatever else has joined it.
fn still_held(
    processes: &[Process],
    members: &[&Process],
    groups: &BTreeSet<u32>,
) -> BTreeSet<u32> {
    let by_pid: HashMap<u32, &Process> =
        members.iter().map(|member| (member.pid, *member)).collect();
    let (kept, _) = Tree::new(processes).split_known(|process| {
        by_pid
            .get(&process.pid)
            .is_some_and(|member| member.started == process.started)
    });
    kept.into_iter()
        .filter(|process| {
            groups.contains(&process.pgid)
                && by_pid
                    .get(&process.pid)
                    .is_some_and(|member| member.pgid == process.pgid)
        })
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

    fn pids(processes: &[&Process]) -> Vec<u32> {
        processes.iter().map(|p| p.pid).collect()
    }

    /// A [`CommandShells`] that has noted each of `listings` in turn.
    fn noted(listings: &[&[Process]]) -> CommandShells {
        let mut shells = CommandShells::new(uuid::Uuid::nil());
        for listing in listings {
            shells.note(listing, AGY);
        }
        shells
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
                exiting: false,
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
        let processes = probe();
        assert_eq!(pids(&Tree::new(&processes).shells_under(AGY)), vec![9022]);
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
        assert_eq!(pids(&Tree::new(&processes).shells_under(AGY)), vec![9022]);
    }

    #[test]
    fn once_agy_is_gone_the_shell_and_what_runs_under_it_are_found_by_start_time() {
        let mut shells = noted(&[&probe()]);
        let reparented = parse_processes(
            " 9022     1  9022 Ss+  s001  Thu Nov 19 23:29:21 2026
 9024  9022  9022 S+   s001  Thu Nov 19 23:29:21 2026
 9030  9024  9030 S+   s001  Thu Nov 19 23:29:30 2026
",
        )
        .unwrap();
        let members = shells.follow(&Tree::new(&reparented));
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
        let processes = debugger_rows(Some(("SXs+", 9031)));
        assert_eq!(
            pids(&Tree::new(&processes).shells_under(AGY)),
            vec![9022],
            "a debugged shell is no command shell"
        );
        let members = noted(&[]).follow_shells(&processes);
        assert_eq!(pids(&members), vec![9022, 9030, 9031]);
        assert_eq!(groups_of(&members, AGY, OWN), BTreeSet::from([9022]));
    }

    /// The command runs `lldb` (9030), whose `debugserver` (9031) attaches to
    /// a terminal shell outside `agy`'s tree (700, running 701). `X` marks 700
    /// as debugged and 650 is its parent; `None` lists it as the debugger's
    /// undebugged child, as a listing does when the attach lands between the
    /// kernel's reads of its traced flag and its parent.
    fn debugger_rows(outside: Option<(&str, u32)>) -> Vec<Process> {
        let (stat, parent) = outside.unwrap_or(("Ss+", 9031));
        parse_processes(&format!(
            " 8970  8966  8970 S      ??  Thu Nov 19 23:29:12 2026
 9022  8970  9022 Ss+  s001  Thu Nov 19 23:29:21 2026
 9030  9022  9022 S+   s001  Thu Nov 19 23:29:22 2026
 9031  9030  9022 S+   s001  Thu Nov 19 23:29:22 2026
  700 {parent:>5}   700 {stat:<4} s002  Thu Nov 19 08:00:01 2026
  701   700   701 S+   s002  Thu Nov 19 08:00:05 2026
"
        ))
        .unwrap()
    }

    #[test]
    fn a_process_listed_mid_attach_is_dropped_once_seen_being_debugged() {
        let racy = debugger_rows(None);
        let attached = debugger_rows(Some(("SXs+", 9031)));
        let detached = debugger_rows(Some(("Ss+", 650)));
        let mut shells = noted(&[&racy, &attached]);
        assert_eq!(
            pids(&shells.follow(&Tree::new(&detached))),
            vec![9022, 9030, 9031],
            "700 and 701 stay dropped after the debugger detaches"
        );
    }

    #[test]
    fn a_debugged_member_is_no_reason_for_sigkill() {
        let racy = debugger_rows(None);
        let attached = debugger_rows(Some(("SXs+", 9031)));
        let members = noted(&[]).follow_shells(&racy);
        assert_eq!(
            still_held(&attached, &members, &BTreeSet::from([9022, 700, 701])),
            BTreeSet::from([9022]),
            "a debugged member, and a member under it, are no reason for SIGKILL"
        );
    }

    /// After `agy` exits: the target `debugserver` (9031) attached to long
    /// before the Stop (700, outside `agy`'s tree, sharing its group with 702)
    /// is exiting or has exited, with `stat` as its state. macOS clears the
    /// traced flag as a process exits and gives it back its own parent only
    /// when the debugger reaps it, so `ps` lists it under the debugger with no
    /// `X`.
    fn exited_target_rows(stat: &str) -> Vec<Process> {
        parse_processes(&format!(
            " 9022     1  9022 Ss+  s001  Thu Nov 19 23:29:21 2026
 9030  9022  9022 S+   s001  Thu Nov 19 23:29:22 2026
 9031  9030  9022 S+   s001  Thu Nov 19 23:29:22 2026
  700  9031   700 {stat:<4}   ??  Thu Nov 19 08:00:01 2026
  702   650   700 S+   s002  Thu Nov 19 08:00:09 2026
"
        ))
        .unwrap()
    }

    /// A zombie, and a process still exiting.
    const EXITED: [&str; 2] = ["Z", "REs"];

    #[test]
    fn an_exited_debug_target_does_not_make_its_group_a_target() {
        for stat in EXITED {
            let mut shells = noted(&[&debugger_rows(Some(("SXs+", 9031)))]);
            let now = exited_target_rows(stat);
            let members = shells.follow_shells(&now);
            assert_eq!(pids(&members), vec![9022, 9030, 9031], "{stat}");
            assert_eq!(
                groups_of(&members, AGY, OWN),
                BTreeSet::from([9022]),
                "{stat}: 702 shares a group with 700 and was never recorded"
            );
        }
    }

    #[test]
    fn an_exited_member_is_no_reason_for_sigkill() {
        let racy = debugger_rows(None);
        let members = noted(&[]).follow_shells(&racy);
        for stat in EXITED {
            assert_eq!(
                still_held(
                    &exited_target_rows(stat),
                    &members,
                    &BTreeSet::from([9022, 700])
                ),
                BTreeSet::from([9022]),
                "{stat}: 702 shares a group with 700 and was never recorded"
            );
        }
    }

    #[test]
    fn a_process_seen_under_a_shell_is_ended_after_the_shell_exits() {
        let mut shells = noted(&[&probe()]);
        // The shell's command finished while `agy` exited, leaving its
        // `sleep` to launchd in the shell's group, with a child of its own.
        let later = parse_processes(concat!(
            " 9024     1  9022 S      ??  Thu Nov 19 23:29:21 2026\n",
            " 9040  9024  9022 S      ??  Thu Nov 19 23:29:40 2026\n",
        ))
        .unwrap();
        let members = shells.follow_shells(&later);
        assert_eq!(pids(&members), vec![9024, 9040]);
        assert_eq!(groups_of(&members, AGY, OWN), BTreeSet::from([9022]));
    }

    #[test]
    fn a_pid_with_another_start_time_is_another_process() {
        let mut shells = noted(&[&probe()]);
        let reused =
            parse_processes(" 9022     1  9022 Ss+  s003  Thu Nov 19 23:40:00 2026\n").unwrap();
        assert!(shells.follow(&Tree::new(&reused)).is_empty());
    }

    #[test]
    fn groups_0_1_agys_and_switchboards_are_never_targets() {
        let members = [AGY, OWN, 0, 1, 9022].map(|pgid| Process {
            pid: 9100 + pgid,
            ppid: 1,
            pgid,
            session_leader: false,
            has_terminal: false,
            traced: false,
            exiting: false,
            started: "Thu Nov 19 23:29:21 2026".to_owned(),
        });
        let members: Vec<&Process> = members.iter().collect();
        assert_eq!(groups_of(&members, AGY, OWN), BTreeSet::from([9022]));
    }

    #[test]
    fn a_group_is_killed_only_while_it_holds_a_recorded_process() {
        let processes = probe();
        let members = noted(&[]).follow_shells(&processes);
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

    impl CommandShells {
        /// Note `processes`, then follow it: the members `end` would act on
        /// were this the listing after `agy` exits.
        fn follow_shells<'a>(&mut self, processes: &'a [Process]) -> Vec<&'a Process> {
            self.note(processes, AGY);
            self.follow(&Tree::new(processes))
        }
    }
}
