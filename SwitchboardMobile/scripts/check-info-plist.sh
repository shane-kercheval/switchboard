#!/bin/sh
# Checks a *built* app's Info.plist — the source plist merged with the
# INFOPLIST_KEY_* build settings — for the keys each configuration must or
# must not ship.
#
#   check-info-plist.sh <path/to/App.app/Info.plist> <Debug|Release>
set -u

plist=$1
configuration=$2
failures=0

fail() {
	echo "$plist ($configuration): $1" >&2
	failures=$((failures + 1))
}

value() {
	plutil -extract "$1" raw -o - "$plist" 2>/dev/null
}

# iOS terminates the app on first use of the camera or Face ID without these.
for key in NSCameraUsageDescription NSFaceIDUsageDescription; do
	[ -n "$(value "$key")" ] || fail "missing $key"
done

case $configuration in
Debug)
	[ "$(value NSAppTransportSecurity.NSAllowsLocalNetworking)" = true ] ||
		fail "missing NSAppTransportSecurity.NSAllowsLocalNetworking"
	[ -n "$(value NSLocalNetworkUsageDescription)" ] || fail "missing NSLocalNetworkUsageDescription"
	;;
Release)
	plutil -extract NSAppTransportSecurity raw -o - "$plist" >/dev/null 2>&1 &&
		fail "carries an NSAppTransportSecurity exception"
	plutil -extract NSLocalNetworkUsageDescription raw -o - "$plist" >/dev/null 2>&1 &&
		fail "carries NSLocalNetworkUsageDescription"
	;;
*)
	fail "unknown configuration"
	;;
esac

[ "$failures" -eq 0 ]
