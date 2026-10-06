import SwiftUI
import SwitchboardMobileKit

struct RootView: View {
    let bindingCheck: BindingSelfCheck

    var body: some View {
        VStack(spacing: 16) {
            Text("Switchboard")
                .font(.largeTitle.bold())
            switch bindingCheck {
            case .derived(let code):
                Text(code)
                    .font(.system(.largeTitle, design: .monospaced))
                    .accessibilityLabel("Sample confirmation code \(code.map(String.init).joined(separator: " "))")
                Text("Sample confirmation code from the shared Rust library")
                    .font(.footnote)
                    .foregroundStyle(.secondary)
            case .failed:
                Label("The shared Rust library failed to run", systemImage: "exclamationmark.triangle")
                    .foregroundStyle(.red)
            }
        }
        .multilineTextAlignment(.center)
        .padding()
    }
}

#Preview("Derived") {
    RootView(bindingCheck: .derived(code: "024132"))
}

#Preview("Failed") {
    RootView(bindingCheck: .failed(.bindingFailure))
}
