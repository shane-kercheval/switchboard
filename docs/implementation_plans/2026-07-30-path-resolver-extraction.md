# PATH resolver extraction

Deferred follow-up from the harness-PATH resolution work. Not scheduled; recorded
so the constraint it imposes on testing is visible to whoever touches this next.

## What

Split `crates/harness/src/subprocess.rs` (~2,000 lines, two lifecycles) into:

- `subprocess.rs` — bounded process execution: `run_bounded`, the stdout reader,
  `terminate_then_kill` / `terminate_group_then_kill`, stderr tail formatting.
- `path_resolver.rs` — the `CaptureState` machine, its statics, and the public
  surface (`resolved_path`, `await_capture`, `ensure_path_settled`,
  `invalidate_path_cache`, `subscribe_revisions`, `path_source*`).

Re-export from `subprocess` so adapter call sites are unchanged.

## Why it matters beyond tidiness

The resolver's state is process-global. That is what makes two call sites
untestable without injected seams:

- `commands.rs::install_status_with` takes a `PathState` so a test can observe
  the snapshot's ordering relative to `probe()`.
- `dispatcher::path_readiness` is swappable so a test can hold readiness at
  `Capturing` without spawning a real login shell or perturbing sibling tests.

Both seams exist to work around the globals. An owned `PathResolver` — held by
`AppState` and passed to the dispatcher — would let those call sites take the
resolver directly, and the seams would collapse into ordinary constructor
injection. Until then, every new consumer of the resolver inherits the same
problem: its tests either mutate global state or prove less than they claim.

## Constraints learned

- The two lifecycles share `READ_DRAIN_GRACE` and `TERMINATE_GRACE`; the split
  must keep `capture_attempt_budget()` deriving from both, since it is what sizes
  the Recheck wait.
- The test module is currently one block covering both lifecycles; it has to be
  divided along the same boundary or the split leaves tests orphaned from the
  code they exercise.
