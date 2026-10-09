#!/bin/sh
# Fails if anything outside the package's tests calls the Mac's halves of the
# handshakes: EncryptedSession.accept and MacPairing. They exist so tests can
# run real round trips through the code the Mac runs; the app never plays the
# Mac, and `internal` alone would let any file in the package call them.
#
# A grep, not a parser, like check-binding-imports.sh: it catches calls
# written the conventional ways.
set -u

root=$(cd "$(dirname "$0")/.." && pwd)
tests="$root/SwitchboardMobileKit/Tests/"
pattern='EncryptedSession[[:space:]]*\.[[:space:]]*accept[[:space:]]*\(|(^|[^A-Za-z0-9_])MacPairing[[:space:]]*(\.[[:space:]]*init[[:space:]]*)?\('

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
match|let (mac, reply) = try EncryptedSession.accept(macKeys: keys, phoneNoisePublicKey: key, message1: m)
match|try EncryptedSession .accept(
match|let mac = try MacPairing(macKeys: keys, preSharedKey: psk, macName: "Mac", message1: m)
match|let mac = try MacPairing.init(macKeys: keys, preSharedKey: psk, macName: "Mac", message1: m)
none|static func accept(
none|final class MacPairing: Sendable {
none|try MacPairingHandshake(keys: macKeys.keys, psk: preSharedKey, macName: macName, message1: message1)
none|let session = try handshake.finish(message2: reply)
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
	echo "Only the tests may call EncryptedSession.accept or construct MacPairing:" >&2
	printf '%s' "$violations" >&2
	exit 1
fi

# The tests do call them, so finding no call there means the scan is not
# looking where they are used.
if [ "$found_in_tests" = no ]; then
	echo "check-test-only-calls: found no call in $tests, so the scan is not covering the package." >&2
	exit 1
fi
