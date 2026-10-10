# Shared by the check-*.sh guards: the two steps that must fail safe the same
# way in each. Each guard keeps its own pattern, cases, and path rules. POSIX
# sh has no local variables, so these use a guard_ prefix to leave the
# caller's alone.

# Checks PATTERN against "expected|line" cases on stdin, where expected is
# `match` or `none`, so a guard cannot pass by matching nothing. NAME prefixes
# the messages.
self_test() {
	guard_name=$1
	guard_pattern=$2
	guard_failures=0
	while IFS='|' read -r guard_expected guard_line; do
		if printf '%s\n' "$guard_line" | grep -qE "$guard_pattern"; then guard_actual=match; else guard_actual=none; fi
		if [ "$guard_actual" != "$guard_expected" ]; then
			echo "$guard_name self-check: expected $guard_expected for: $guard_line" >&2
			guard_failures=$((guard_failures + 1))
		fi
	done
	[ "$guard_failures" -eq 0 ]
}

# Prints every Swift file under ROOT matching PATTERN, skipping build output.
# One recursive grep, whose own status separates "no match" (1) from an error
# (2) such as an unreadable file; an error fails rather than passing as clean.
scan() {
	guard_name=$1
	guard_pattern=$2
	guard_root=$3
	grep -rlE --include='*.swift' \
		--exclude-dir=.build --exclude-dir=.swiftpm --exclude-dir=DerivedData \
		"$guard_pattern" "$guard_root"
	guard_status=$?
	if [ "$guard_status" -gt 1 ]; then
		echo "$guard_name: the scan failed (grep exit $guard_status), so nothing was checked." >&2
		return 1
	fi
	return 0
}
