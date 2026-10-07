import Foundation
import os

extension Logger {
    static let crypto = Logger(subsystem: Bundle.main.bundleIdentifier ?? "SwitchboardMobile", category: "crypto")
}
