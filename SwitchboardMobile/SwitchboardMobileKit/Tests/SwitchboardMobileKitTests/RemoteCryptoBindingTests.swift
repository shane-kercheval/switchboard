import Foundation
import Testing
@testable import SwitchboardMobileKit

struct RemoteCryptoBindingTests {
    /// The expected code is the Rust crate's own pinned vector, so a pass means
    /// Swift ran the Rust derivation rather than something that merely looks
    /// like a code.
    @Test func derivesTheCodeTheMacDerives() throws {
        let code = try RemoteCryptoBinding().confirmationCode(handshakeHash: Data(repeating: 0, count: 32))
        #expect(code == "024132")
    }

    @Test func rustErrorSurfacesAsThrow() {
        #expect(throws: ConfirmationCodeError.invalidHandshakeHash(length: 3)) {
            try RemoteCryptoBinding().confirmationCode(handshakeHash: Data([1, 2, 3]))
        }
    }

    @Test func selfCheckReportsTheDerivedCode() {
        #expect(BindingSelfCheck(using: RemoteCryptoBinding()) == .derived(code: "024132"))
    }

    @Test func selfCheckReportsAFailingBinding() {
        #expect(BindingSelfCheck(using: FailingCodes()) == .failed(.bindingFailure))
    }
}

private struct FailingCodes: ConfirmationCodeDeriving {
    func confirmationCode(handshakeHash: Data) throws(ConfirmationCodeError) -> String {
        throw .bindingFailure
    }
}
