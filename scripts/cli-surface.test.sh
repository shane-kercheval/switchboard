#!/usr/bin/env bash
# Tests for scripts/cli-surface.sh and `make cli-surface`. Fake `codex`,
# `claude` and `agy` executables shadow the real ones on PATH, so these run
# without any harness installed or logged in. Each fake's behavior is steered
# by environment variables:
#   FAKE_FAIL_ARGS    the exact argument string the fake should fail on
#   FAKE_FAIL_STATUS  the exit code for that failure (default 1); the fake
#                     prints a Go-flag-style error line before the usage text
#   FAKE_EMPTY        make the fake's command table empty (no methods, no init
#                     event, no /help command list)
#
# Run: make test-scripts
set -euo pipefail

repo=$(cd "$(dirname "$0")/.." && pwd)
script="$repo/scripts/cli-surface.sh"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
bin="$tmp/bin"
mkdir "$bin"
failures=0

# Answers help only through `codex help <command>`, the form that cannot run a
# command; `<command> --help` falls through to "unexpected arguments".
cat >"$bin/codex" <<'FAKE'
#!/bin/sh
[ "$*" = "${FAKE_FAIL_ARGS:-}" ] && { echo "boom" >&2; exit 1; }
method() { printf '{"properties":{"method":{"enum":["%s"]}}}' "$1"; }
# Writes ClientRequest.json into $1. With $2 set, adds two experimental methods
# (one listed twice), as `--experimental` adds them to the stable set.
schema() {
  mkdir -p "$1"
  if [ -n "${FAKE_EMPTY:-}" ]; then
    echo '{"oneOf":[]}' >"$1/ClientRequest.json"
    return
  fi
  extra=""
  [ -n "${2:-}" ] && extra=",$(method thread/readState/update),$(method thread/items/read),$(method thread/items/read)"
  # One method per entry, as in Codex's schema, including names with no
  # slash, a duplicate, and an unrelated enum that must not be listed.
  printf '{"oneOf":[%s,%s,%s,%s%s],"definitions":{"Mode":{"enum":["read/only"]}}}\n' \
    "$(method thread/start)" "$(method initialize)" \
    "$(method account/rateLimits/read)" "$(method thread/start)" "$extra" >"$1/ClientRequest.json"
}
case "$*" in
  "--version") echo "codex-cli 9.9.9" ;;
  "help")
    printf 'Usage: codex [COMMAND]\n\nCommands:\n'
    printf '  exec            Run non-interactively\n'
    printf '  plugin|plugins  Manage plugins\n'
    printf '  help            Print this message\n'
    printf '\nOptions:\n  -h, --help  width=%s\n' "${COLUMNS:-unset}"
    ;;
  "help exec") printf 'Usage: codex exec\n\nCommands:\n  fork  Fork a session\n' ;;
  "help exec fork") echo "Usage: codex exec fork <SESSION_ID>" ;;
  "help plugin") echo "Usage: codex plugin" ;;
  "app-server generate-json-schema --out "*) schema "$4" ;;
  "app-server generate-json-schema --experimental --out "*) schema "$5" experimental ;;
  *) echo "fake codex: unexpected arguments: $*" >&2; exit 64 ;;
esac
FAKE

# Refuses a help call made outside a throwaway config folder, and a print-mode
# call that would save a session.
cat >"$bin/claude" <<'FAKE'
#!/bin/sh
[ "$*" = "${FAKE_FAIL_ARGS:-}" ] && { echo "boom" >&2; exit 1; }
case "$*" in
  *--help)
    [ -n "${CLAUDE_CONFIG_DIR:-}" ] && [ -d "$CLAUDE_CONFIG_DIR" ] &&
      [ -z "$(ls -A "$CLAUDE_CONFIG_DIR")" ] ||
      { echo "fake claude: help asked outside an empty throwaway config" >&2; exit 65; }
    ;;
  "-p "*)
    case "$*" in *--no-session-persistence*) ;; *)
      echo "fake claude: print mode without --no-session-persistence" >&2; exit 66 ;;
    esac
    ;;
esac
case "$*" in
  "--version") echo "9.9.9 (Claude Code)" ;;
  "--help") printf 'Usage: claude\n\nCommands:\n  mcp  Configure MCP servers\n' ;;
  "mcp --help") echo "Usage: claude mcp" ;;
  "-p "*)
    [ -z "${FAKE_EMPTY:-}" ] &&
      echo '{"type":"system","subtype":"init","slash_commands":["context","compact"]}'
    echo '{"type":"result","subtype":"success","num_turns":0}'
    ;;
  *) echo "fake claude: unexpected arguments: $*" >&2; exit 64 ;;
esac
FAKE

cat >"$bin/agy" <<'FAKE'
#!/bin/sh
if [ "$*" = "${FAKE_FAIL_ARGS:-}" ]; then
  printf 'flag provided but not defined: -x\nUsage of agy:\n'
  exit "${FAKE_FAIL_STATUS:-1}"
fi
case "$*" in
  "--version") echo "9.9.9" ;;
  "--help")
    printf 'Usage of agy:\n  --print  Run one prompt\n\nAvailable subcommands:\n'
    printf '  mcp     Manage MCP servers\n  danger  Change things\n'
    printf '  update  Update CLI\n  help    Show help\n'
    ;;
  "help mcp")
    printf 'Usage: agy mcp\n\nAvailable subcommands:\n  list  List servers\n\n'
    printf 'Run "agy mcp <subcommand> --help" for flags.\n'
    ;;
  "help danger")
    # The normal heading but no promise that `--help` is safe, like a group
    # whose subcommands would run instead of printing help.
    printf 'Usage: agy danger <command>\n\nAvailable subcommands:\n  wipe  Remove everything\n'
    ;;
  "danger wipe --help") touch "$FAKE_MARKER"; echo "wiped" ;;
  "help update") echo "Usage of update:"; exit 2 ;;
  "mcp list --help") echo "Usage: agy mcp list" ;;
  "-p /help --output-format json")
    if [ -n "${FAKE_EMPTY:-}" ]; then echo '{"status":"SUCCESS","response":""}'; else
      echo '{"command":{"data":{"commands":[{"name":"usage","description":"View quota"}]}}}'
    fi
    ;;
  *) echo "fake agy: unexpected arguments: $*" >&2; exit 64 ;;
esac
FAKE
chmod +x "$bin/codex" "$bin/claude" "$bin/agy"

out="$tmp/out" err="$tmp/err"
export FAKE_MARKER="$tmp/danger-wipe-ran"

# Run the script with the fakes first on PATH. Leaves stdout in $out, stderr
# in $err, and returns the script's exit status.
capture() {
  PATH="$bin:$PATH" "$script" "$@" >"$out" 2>"$err"
}

pass() { echo "ok   $1"; }
fail_test() {
  echo "FAIL $1: $2"
  sed 's/^/     | /' "$err"
  failures=$((failures + 1))
}

expect_success() {
  local name="$1"
  shift
  if capture "$@"; then return 0; fi
  fail_test "$name" "script failed"
  return 1
}

expect_failure() {
  local name="$1" message="$2"
  shift 2
  if capture "$@"; then
    fail_test "$name" "script succeeded, expected failure"
  elif ! grep -qF "$message" "$err"; then
    fail_test "$name" "stderr does not name the failure: $message"
  else
    pass "$name"
  fi
}

has_line() { grep -qxF "$1" "$out"; }

# --- Codex ---------------------------------------------------------------

name="codex: walks every subcommand level and skips help"
if expect_success "$name" codex; then
  if has_line "version: codex-cli 9.9.9" &&
    has_line "===== codex exec fork --help" &&
    has_line "===== codex plugin --help" &&
    ! grep -qF "===== codex help --help" "$out"; then
    pass "$name"
  else
    fail_test "$name" "missing or unexpected sections"
  fi
fi

name="codex: help width is pinned whatever the terminal says"
if COLUMNS=200 expect_success "$name" codex; then
  if grep -qF "width=100" "$out"; then pass "$name"; else fail_test "$name" "width not 100"; fi
fi

name="codex: lists every app-server method, sorted and de-duplicated, and nothing else"
if expect_success "$name" codex; then
  table=$(sed -n '/^===== codex app-server methods (ClientRequest)$/,/^===== /p' "$out" | sed '1d;/^===== /d;/^$/d')
  expected=$(printf 'account/rateLimits/read\ninitialize\nthread/start')
  if [[ $table == "$expected" ]]; then pass "$name"; else fail_test "$name" "got: $table"; fi
fi

name="codex: lists only the experimental app-server methods in their own table, sorted and de-duplicated"
if expect_success "$name" codex; then
  table=$(sed -n '/^===== codex app-server methods (ClientRequest, experimental only)$/,$p' "$out" | tail -n +2)
  expected=$(printf 'thread/items/read\nthread/readState/update')
  if [[ $table == "$expected" ]]; then pass "$name"; else fail_test "$name" "got: $table"; fi
fi

FAKE_FAIL_ARGS="help exec fork" expect_failure \
  "codex: a failed nested help call stops the script and names it" \
  "help for 'codex exec fork' failed" codex

FAKE_FAIL_ARGS="--version" expect_failure \
  "codex: a failed version call stops the script" \
  "'codex --version' failed" codex

FAKE_EMPTY=1 expect_failure \
  "codex: an empty method list is refused" \
  "codex app-server methods (ClientRequest) came back empty" codex

# --- Claude Code -------------------------------------------------------

name="claude: asks for help in a throwaway config and lists print-mode slash commands"
if expect_success "$name" claude; then
  if has_line "===== claude mcp --help" && has_line "compact" && has_line "context"; then
    pass "$name"
  else
    fail_test "$name" "missing sections or slash commands"
  fi
fi

FAKE_EMPTY=1 expect_failure \
  "claude: a stream with no init event is refused" \
  "claude slash commands accepted in print mode (bare) came back empty" claude

# --- Antigravity -------------------------------------------------------

name="agy: accepts help that exits 2 and walks nested subcommands"
if expect_success "$name" agy; then
  if has_line "===== agy help update" && has_line "Usage of update:" &&
    has_line "===== agy mcp list --help" && grep -qF "/usage" "$out"; then
    pass "$name"
  else
    fail_test "$name" "missing sections or slash commands"
  fi
fi

name="agy: never walks a group whose help does not promise --help is safe"
rm -f "$FAKE_MARKER"
if expect_success "$name" agy; then
  if [[ -e $FAKE_MARKER ]]; then
    fail_test "$name" "the group's subcommand was run"
  elif ! has_line "===== agy help danger" || grep -qF "===== agy danger wipe --help" "$out"; then
    fail_test "$name" "expected the group's help and none of its subcommands"
  else
    pass "$name"
  fi
fi

FAKE_FAIL_ARGS="help mcp" expect_failure \
  "agy: a help call exiting 1 stops the script" \
  "'agy help mcp' failed (exit 1)" agy

FAKE_FAIL_ARGS="help mcp" FAKE_FAIL_STATUS=2 expect_failure \
  "agy: exit 2 from a help call other than 'help update' stops the script" \
  "'agy help mcp' failed (exit 2)" agy

FAKE_FAIL_ARGS="mcp list --help" FAKE_FAIL_STATUS=2 expect_failure \
  "agy: exit 2 from a nested --help call stops the script" \
  "'agy mcp list --help' failed (exit 2)" agy

FAKE_FAIL_ARGS="help update" FAKE_FAIL_STATUS=2 expect_failure \
  "agy: 'help update' exiting 2 with an error line first stops the script" \
  "'agy help update' failed (exit 2)" agy

FAKE_EMPTY=1 expect_failure \
  "agy: a /help response without a command list is refused" \
  "agy slash commands (/help) came back empty" agy

# --- make cli-surface --------------------------------------------------

snapshots="$tmp/snapshots"
mkdir "$snapshots"

name="make: a failed capture leaves the previous snapshot untouched"
echo "previous snapshot" >"$snapshots/codex.txt"
if PATH="$bin:$PATH" FAKE_FAIL_ARGS="help exec" \
  make -s -C "$repo" cli-surface HARNESS=codex CLI_SURFACE_DIR="$snapshots" >"$out" 2>"$err"; then
  fail_test "$name" "make succeeded, expected failure"
elif [[ $(cat "$snapshots/codex.txt") != "previous snapshot" ]]; then
  fail_test "$name" "snapshot was overwritten"
elif [[ -e "$snapshots/codex.txt.tmp" ]]; then
  fail_test "$name" "temporary file left behind"
else
  pass "$name"
fi

name="make: a successful capture replaces the snapshot"
if PATH="$bin:$PATH" make -s -C "$repo" cli-surface HARNESS=codex CLI_SURFACE_DIR="$snapshots" \
  >"$out" 2>"$err" && grep -qxF "version: codex-cli 9.9.9" "$snapshots/codex.txt"; then
  pass "$name"
else
  fail_test "$name" "snapshot not replaced"
fi

if ((failures > 0)); then
  echo "$failures test(s) failed"
  exit 1
fi
echo "all cli-surface tests passed"
