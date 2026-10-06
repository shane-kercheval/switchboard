#!/bin/sh
# Fails if any Swift file outside SwitchboardMobileKit's Crypto/ imports the
# UniFFI-generated module or the C module under it. Generated types stop at
# Crypto/, so regenerating the bindings can only ever break that directory.
#
# A grep, not a parser: two imports on one line separated by `;` get past it.
set -u

root=$(cd "$(dirname "$0")/.." && pwd)
allowed="$root/SwitchboardMobileKit/Sources/SwitchboardMobileKit/Crypto/"
pattern='^[[:space:]]*(@[A-Za-z_]+(\([^)]*\))?[[:space:]]+)*([a-z]+[[:space:]]+)?import[[:space:]]+([a-z]+[[:space:]]+)?(SwitchboardRemoteCrypto|switchboard_remote_cryptoFFI)([^A-Za-z0-9_]|$)'

# The pattern is checked against known lines first, so this guard cannot pass
# by matching nothing.
self_check_failures=0
while IFS='|' read -r expected line; do
	if printf '%s\n' "$line" | grep -qE "$pattern"; then actual=match; else actual=none; fi
	if [ "$actual" != "$expected" ]; then
		echo "check-binding-imports self-check: expected $expected for: $line" >&2
		self_check_failures=$((self_check_failures + 1))
	fi
done <<'CASES'
match|import SwitchboardRemoteCrypto
match|internal import SwitchboardRemoteCrypto
match|    public import SwitchboardRemoteCrypto
match|@testable import SwitchboardRemoteCrypto
match|@preconcurrency import SwitchboardRemoteCrypto
match|@_implementationOnly import SwitchboardRemoteCrypto
match|@_spi(Internal) @testable import SwitchboardRemoteCrypto
match|import struct SwitchboardRemoteCrypto.CryptoError
match|import func SwitchboardRemoteCrypto.confirmationCode
match|import switchboard_remote_cryptoFFI
match|@preconcurrency import switchboard_remote_cryptoFFI
none|// import SwitchboardRemoteCrypto
none|let source = "import SwitchboardRemoteCrypto"
none|import SwitchboardMobileKit
none|import SwitchboardRemoteCryptoExtras
CASES
[ "$self_check_failures" -eq 0 ] || exit 1

violations=$(grep -rlE --include='*.swift' "$pattern" \
	"$root/SwitchboardMobileKit/Sources" "$root/SwitchboardMobileKit/Tests" "$root/SwitchboardMobile" |
	grep -v "^$allowed")
if [ -n "$violations" ]; then
	echo "Only SwitchboardMobileKit/Sources/SwitchboardMobileKit/Crypto/ may import the generated bindings:" >&2
	echo "$violations" >&2
	exit 1
fi
