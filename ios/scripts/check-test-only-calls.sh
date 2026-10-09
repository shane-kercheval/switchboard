#!/bin/sh
# Fails if any Swift file outside the package's tests names the Mac's halves of
# the handshakes, the generated `MacPairingHandshake` and `acceptSession`. The
# tests use them to run real round trips through the code the Mac runs; the
# app never plays the Mac. The bindings ship whole in one xcframework, so the
# package could still call them; this is what stops it.
#
# A grep for the names, not a module boundary: it catches any spelling of a
# call, an alias, or a reference, but a Mac-only function exported later must
# be added to the pattern by hand.
set -u

root=$(cd "$(dirname "$0")/.." && pwd)
tests="$root/SwitchboardMobileKit/Tests/"
pattern='(^|[^A-Za-z0-9_])(MacPairingHandshake|acceptSession)([^A-Za-z0-9_]|$)'

# The pattern is checked against known lines first, so this guard cannot pass
# by matching nothing.
self_check_failures=0
while IFS='|' read -r expected line; do
	if printf '%s\n' "$line" | grep -qE "$pattern"; then actual=match; else actual=none; fi
	if [ "$actual" != "$expected" ]; then
		echo "check-test-only-calls self-check: expected $expected for: $line" >&2
		self_check_failures=$((self_check_failures + 1))
	fi
done <<'CASES'
match|try MacPairingHandshake(keys: k, psk: p, macName: "Mac", message1: m)
match|let make = MacPairingHandshake.init
match|try acceptSession(keys: k, phoneNoisePublicKey: key, message1: m)
match|let accept = acceptSession
match|    handshake: MacPairingHandshake
match|try SwitchboardRemoteCrypto.acceptSession(
none|let handshake = try PairingHandshake(keys: k, macNoisePublicKey: key, psk: p)
none|let acceptSessions = 2
none|let isMacPairingHandshakeDone = true
CASES
[ "$self_check_failures" -eq 0 ] || exit 1

# One recursive grep, with grep's own status separating "no match" (1) from an
# error (2); generated bindings are skipped by exact path.
matches=$(grep -rlE --include='*.swift' \
	--exclude-dir=.build --exclude-dir=.swiftpm --exclude-dir=DerivedData \
	"$pattern" "$root")
status=$?
if [ "$status" -gt 1 ]; then
	echo "check-test-only-calls: the scan failed (grep exit $status), so nothing was checked." >&2
	exit 1
fi

found_in_tests=no
violations=
while IFS= read -r file; do
	[ -n "$file" ] || continue
	case $file in
	"$root/SwitchboardMobileKit/Generated/"*) ;;
	"$tests"*) found_in_tests=yes ;;
	*) violations="$violations$file
" ;;
	esac
done <<SCAN
$matches
SCAN

if [ -n "$violations" ]; then
	echo "Only the tests may name MacPairingHandshake or acceptSession:" >&2
	printf '%s' "$violations" >&2
	exit 1
fi

# The tests do use them, so finding neither there means the scan is not
# looking where they are used.
if [ "$found_in_tests" = no ]; then
	echo "check-test-only-calls: found no use in $tests, so the scan is not covering the package." >&2
	exit 1
fi
