#!/usr/bin/env bash
# Print a harness CLI's command surface: every command and subcommand's help
# text, plus the harness's other command tables (Codex's app-server methods,
# Claude's and Antigravity's slash commands). `make cli-surface` writes the
# output to docs/research/cli-surface/<harness>.txt, so a harness update
# review can `git diff` the file to see every new command, flag and method.
#
# Usage: scripts/cli-surface.sh codex|claude|agy
#
# Every call is free and changes nothing of Switchboard's: `--help`, a schema
# dump, and slash commands that answer without a model call (`num_turns: 0`).
# One side effect belongs to agy: starting `agy -p` runs agy's own
# auto-updater (at most every 15 minutes), as any agy session does, so agy may
# update itself in the background while this runs.
#
# Any failed call or empty command table stops the script with a message
# naming it, so a partial capture can never replace a good snapshot.
# scripts/cli-surface.test.sh covers those paths with fake CLIs.
set -euo pipefail

harness="${1:?usage: $0 codex|claude|agy}"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir "$work/cwd"

fail() {
  echo "cli-surface: $*" >&2
  exit 1
}

# Run a help command with a fixed width and no terminal on stdin. Codex's help
# wraps to `COLUMNS`, so a developer's terminal width would otherwise reflow
# the whole snapshot and bury real changes in the diff.
run_help() {
  COLUMNS=100 "$@" </dev/null 2>&1
}

# Subcommand names from a help text's command list. The list starts at a line
# matching $2 and ends at the next unindented line; each entry is indented two
# spaces. Aliases written `plugin|plugins` keep the first name. The trailing
# `|| true` is for commands with no subcommands, where `grep` finds nothing
# and exits 1.
subcommands() {
  local help="$1" heading="$2"
  awk -v heading="$heading" '
    $0 ~ heading { inside = 1; next }
    inside && /^[^ ]/ { inside = 0 }
    inside && /^  [a-z]/ { split($1, names, "|"); print names[1] }
  ' <<<"$help" | grep -v -x 'help' || true
}

section() {
  printf '\n===== %s\n' "$1"
}

print_version() {
  local version
  version=$(run_help "$@" --version) || fail "'$* --version' failed"
  echo "version: $version"
}

# Print a command table, refusing an empty one: an empty table would show
# every command as removed in the next review's diff.
print_table() {
  local title="$1" table="$2"
  [[ -n $table ]] || fail "$title came back empty"
  section "$title"
  echo "$table"
}

# Print `<command> --help` and recurse into every subcommand it lists, up to
# four levels deep. Only for CLIs whose `--help` is handled before any
# subcommand runs (Codex's clap, Claude's commander).
walk() {
  local help sub
  help=$(run_help "$@" --help) || fail "'$* --help' failed"
  section "$* --help"
  echo "$help"
  if (($# >= 4)); then
    return
  fi
  for sub in $(subcommands "$help" '^Commands:'); do
    walk "$@" "$sub"
  done
}

codex_surface() {
  print_version codex
  walk codex
  local methods
  codex app-server generate-json-schema --out "$work/schema" >/dev/null 2>&1 ||
    fail "'codex app-server generate-json-schema' failed"
  methods=$(jq -r '.. | objects | select(has("enum")) | .enum[]? | strings | select(contains("/"))' \
    "$work/schema/ClientRequest.json" | sort -u) ||
    fail "could not read app-server methods from ClientRequest.json"
  print_table "codex app-server methods (ClientRequest)" "$methods"
}

claude_surface() {
  print_version claude
  walk claude
  # `--bare --strict-mcp-config` drops the developer's own skills, plugins and
  # MCP prompts, leaving what Claude Code itself accepts in print mode. A
  # command can be missing here and still exist interactively, and account
  # gating can add or remove a few.
  local stream commands
  stream=$(cd "$work/cwd" && claude -p --bare --strict-mcp-config --output-format stream-json \
    --verbose "/cost" </dev/null 2>/dev/null) || fail "'claude -p /cost' failed"
  commands=$(jq -r 'select(.type == "system" and .subtype == "init") | .slash_commands[]?' \
    <<<"$stream" | sort) || fail "could not read Claude's init event"
  print_table "claude slash commands accepted in print mode (bare)" "$commands"
}

# Run an agy help command into $agy_help_text. Every agy help call must exit 0
# with output, with one observed exception: `agy help update` prints
# `Usage of update:` and exits 2 (agy 1.2.10 and 1.3.3). That case is accepted
# only for that exact call and first line, so an error that also exits 2 is
# never saved as help. A new command that behaves the same way stops the
# capture until it is reviewed and added here.
agy_help() {
  local status=0
  agy_help_text=$(run_help agy "$@") || status=$?
  if ((status == 2)) && [[ "$*" == "help update" && $agy_help_text == "Usage of update:"* ]]; then
    return
  fi
  if ((status != 0)) || [[ -z $agy_help_text ]]; then
    fail "'agy $*' failed (exit $status)"
  fi
}

agy_surface() {
  print_version agy
  local top help sub second json commands
  agy_help --help
  top=$agy_help_text
  section "agy --help"
  echo "$top"
  # `agy help <sub>` rather than `agy <sub> --help` at the top level: it is
  # the documented help path, and it cannot run a subcommand such as `update`
  # by accident.
  for sub in $(subcommands "$top" '^Available subcommands:'); do
    agy_help help "$sub"
    help=$agy_help_text
    section "agy help $sub"
    echo "$help"
    # Descend only where agy promises `--help` is safe: its help for `mcp` and
    # `remote-control` ends with `Run "agy <group> <subcommand> --help" for
    # flags`. `agy plugin` makes no such promise, and its subcommands treat
    # `--help` as an ordinary argument (verified on 1.3.3: `agy plugin list
    # --help` runs `list`, and `agy plugin import --help` tries to import a
    # path named `--help`), so walking it could install or remove a plugin.
    # Its subcommands still appear in the diff through the group's own list.
    if [[ $help != *"Run \"agy $sub <subcommand> --help\""* ]]; then
      continue
    fi
    for second in $(subcommands "$help" '^Available subcommands:'); do
      agy_help "$sub" "$second" --help
      section "agy $sub $second --help"
      echo "$agy_help_text"
    done
  done
  json=$(cd "$work/cwd" && agy -p "/help" --output-format json </dev/null 2>/dev/null) ||
    fail "'agy -p /help' failed"
  commands=$(jq -r '.command.data.commands[]? | "/\(.name)\t\(.description)"' <<<"$json") ||
    fail "could not read agy's /help response"
  print_table "agy slash commands (/help)" "$commands"
}

case "$harness" in
  codex) codex_surface ;;
  claude) claude_surface ;;
  agy) agy_surface ;;
  *)
    echo "unknown harness: $harness (expected codex, claude or agy)" >&2
    exit 2
    ;;
esac
