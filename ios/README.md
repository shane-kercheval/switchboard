# Switchboard for iPhone

`SwitchboardMobile` is an iPhone app for driving Switchboard remotely; it is early scaffolding, not yet usable. Its design is [`docs/implementation_plans/2026-09-29-remote-control.md`](../docs/implementation_plans/2026-09-29-remote-control.md) §7.

- `SwitchboardMobile.xcodeproj` and `SwitchboardMobile/` — a thin SwiftUI app target.
- `SwitchboardMobileKit/` — the local Swift package holding everything else. It links the shared Rust crate [`crates/remote-crypto`](../crates/remote-crypto/) as an xcframework.
- `Config/` — signing settings. `scripts/` — the checks `make check-ios` runs.

## Prerequisites

Full **Xcode 16 or newer** (not just the Command Line Tools) with at least one iPhone simulator installed, plus the root README's [Install](../README.md#install) prerequisites. The pinned Rust toolchain adds the two iOS targets it builds for on its own.

## Commands

Run from the **repo root**:

```sh
make ios-crypto   # build the shared Rust crypto library and its Swift bindings
make check-ios    # build the app and run its tests on the newest iPhone simulator
```

Run `make ios-crypto` before opening `ios/SwitchboardMobile.xcodeproj` in Xcode, and again after changing `crates/remote-crypto`: the library and bindings it generates are not committed, so the project won't resolve its package without them. `check-ios` runs as its own CI job, so `make check` doesn't cover it — run it when a change touches `ios/` or `crates/remote-crypto`. Pass `IOS_SIMULATOR_ID=<udid>` to pick a simulator.

## Running on your iPhone

None of the above needs an Apple Developer account: simulator builds aren't signed. To run the app on your own iPhone, copy `Config/Local.xcconfig.example` to `Config/Local.xcconfig` (gitignored) and set your team id and a bundle id of your own there. A free Apple ID's personal team works for your own device, with apps expiring after 7 days. If you set one up before the app moved to `ios/`, it is still at `SwitchboardMobile/Config/Local.xcconfig`, because git doesn't move ignored files: move it to `ios/Config/`.
