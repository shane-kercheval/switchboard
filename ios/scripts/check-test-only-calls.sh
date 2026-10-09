#!/bin/sh
# Fails if any Swift file outside the package's tests names the Mac's halves of
# the handshakes, the generated `MacPairingHandshake` class and `acceptSession`
# function. The tests use them to run real round trips through the code the
# Mac runs; the app never plays the Mac. The bindings ship whole in one xcframework, so the
# package could still call them; this is what stops it.
#
# A grep for the names, not a module boundary. It catches the Swift names
# however they are written — a call, an alias, a reference — and the C
# functions UniFFI generates for them, which `Crypto/` could call through the
# C module. A Mac-only export added later must be added to the pattern by
# hand.
set -u

. "$(dirname "$0")/lib/guard.sh"

root=$(cd "$(dirname "$0")/.." && pwd)
tests="$root/SwitchboardMobileKit/Tests/"
pattern='(^|[^A-Za-z0-9_])(MacPairingHandshake|acceptSession|uniffi_switchboard_remote_crypto_[a-z]+_[a-z]+_(accept_session|macpairinghandshake)[a-z0-9_]*)([^A-Za-z0-9_]|$)'

# The pattern is checked against known lines first, so this guard cannot pass
# by matching nothing.
self_test check-test-only-calls "$pattern" <<'CASES' || exit 1
match|try MacPairingHandshake(keys: k, psk: p, macName: "Mac", message1: m)
match|let make = MacPairingHandshake.init
match|try acceptSession(keys: k, phoneNoisePublicKey: key, message1: m)
match|let accept = acceptSession
match|    handshake: MacPairingHandshake
match|try SwitchboardRemoteCrypto.acceptSession(
match|uniffi_switchboard_remote_crypto_fn_func_accept_session(keys, key, message, &status)
match|uniffi_switchboard_remote_crypto_fn_constructor_macpairinghandshake_new(
match|uniffi_switchboard_remote_crypto_fn_method_macpairinghandshake_finish(handle, m, &status)
match|uniffi_switchboard_remote_crypto_checksum_func_accept_session()
none|uniffi_switchboard_remote_crypto_fn_func_frame_kind(tag, &status)
none|uniffi_switchboard_remote_crypto_fn_constructor_pairinghandshake_new(
none|let handshake = try PairingHandshake(keys: k, macNoisePublicKey: key, psk: p)
none|let acceptSessions = 2
none|let isMacPairingHandshakeDone = true
CASES

# Generated bindings are skipped by exact path.
matches=$(scan check-test-only-calls "$pattern" "$root") || exit 1

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
	echo "Only the tests may name the Mac-side MacPairingHandshake class or acceptSession function, or their generated C functions:" >&2
	printf '%s' "$violations" >&2
	exit 1
fi

# The tests do use them, so finding neither there means the scan is not
# looking where they are used.
if [ "$found_in_tests" = no ]; then
	echo "check-test-only-calls: found no use in $tests, so the scan is not covering the package." >&2
	exit 1
fi
