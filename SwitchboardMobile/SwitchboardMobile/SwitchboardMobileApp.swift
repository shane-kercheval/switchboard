import SwiftUI
import SwitchboardMobileKit

/// The composition root: the only place concrete dependencies are constructed.
@main
struct SwitchboardMobileApp: App {
    private let bindingCheck = BindingSelfCheck(using: RemoteCryptoBinding())

    var body: some Scene {
        WindowGroup {
            RootView(bindingCheck: bindingCheck)
        }
    }
}
