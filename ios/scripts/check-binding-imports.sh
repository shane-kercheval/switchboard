#!/bin/sh
# Fails if any Swift file other than SwitchboardMobileKit's Crypto/ and the
# tests' MacHalves.swift imports the UniFFI-generated module or the C module
# under it. Generated types stop there, so regenerating the bindings can only
# ever break those files. MacHalves.swift imports them on purpose: it holds
# the Mac's test-only handshake halves, and tests don't ship.
#
# A grep, not a parser: it catches every conventional import form, but two
# imports on one line separated by `;` get past it.
set -u

. "$(dirname "$0")/lib/guard.sh"

root=$(cd "$(dirname "$0")/.." && pwd)
allowed="$root/SwitchboardMobileKit/Sources/SwitchboardMobileKit/Crypto/"
mac_halves="$root/SwitchboardMobileKit/Tests/SwitchboardMobileKitTests/MacHalves.swift"
pattern='^[[:space:]]*(@[A-Za-z_]+(\([^)]*\))?[[:space:]]+)*([a-z]+[[:space:]]+)?import[[:space:]]+([a-z]+[[:space:]]+)?(SwitchboardRemoteCrypto|switchboard_remote_cryptoFFI)([^A-Za-z0-9_]|$)'

# The pattern is checked against known lines first, so this guard cannot pass
# by matching nothing.
self_test check-binding-imports "$pattern" <<'CASES' || exit 1
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

# The scan covers the whole app tree, so a new target or folder is covered
# without editing this list. Matches in the bindings themselves, under
# SwitchboardMobileKit/Generated/, are skipped by exact path.
matches=$(scan check-binding-imports "$pattern" "$root") || exit 1

found_allowed=no
violations=
while IFS= read -r file; do
	[ -n "$file" ] || continue
	case $file in
	"$root/SwitchboardMobileKit/Generated/"* | "$mac_halves") ;;
	"$allowed"*) found_allowed=yes ;;
	*) violations="$violations$file
" ;;
	esac
done <<SCAN
$matches
SCAN

if [ -n "$violations" ]; then
	echo "Only SwitchboardMobileKit/Sources/SwitchboardMobileKit/Crypto/ and Tests/SwitchboardMobileKitTests/MacHalves.swift may import the generated bindings:" >&2
	printf '%s' "$violations" >&2
	exit 1
fi

# Crypto/ itself imports the bindings, so finding no import there means the
# scan is not looking where the bindings are used.
if [ "$found_allowed" = no ]; then
	echo "check-binding-imports: found no import of the bindings in $allowed, so the scan is not covering the package." >&2
	exit 1
fi
