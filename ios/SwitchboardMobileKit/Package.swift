// swift-tools-version: 6.0

import PackageDescription

let package = Package(
    name: "SwitchboardMobileKit",
    platforms: [.iOS(.v17)],
    products: [
        .library(name: "SwitchboardMobileKit", targets: ["SwitchboardMobileKit"]),
    ],
    targets: [
        .target(
            name: "SwitchboardMobileKit",
            dependencies: ["SwitchboardRemoteCrypto"]
        ),
        // `Generated/` is written by `make ios-crypto` from `crates/remote-crypto`
        // and is not committed. Among the package's sources only `Crypto/`
        // imports this module; among the tests, only `MacHalves.swift`.
        .target(
            name: "SwitchboardRemoteCrypto",
            dependencies: ["SwitchboardRemoteCryptoFFI"],
            path: "Generated/SwitchboardRemoteCrypto"
        ),
        .binaryTarget(
            name: "SwitchboardRemoteCryptoFFI",
            path: "Generated/SwitchboardRemoteCryptoFFI.xcframework"
        ),
        .testTarget(
            name: "SwitchboardMobileKitTests",
            // The Mac's test-only halves call the bindings directly.
            dependencies: ["SwitchboardMobileKit", "SwitchboardRemoteCrypto"]
        ),
    ]
)
