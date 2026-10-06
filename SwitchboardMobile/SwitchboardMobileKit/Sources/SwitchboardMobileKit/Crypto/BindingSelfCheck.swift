import Foundation
import os

/// Proof, at launch, that the Rust library linked and runs: a confirmation
/// code derived from a fixed handshake hash.
public enum BindingSelfCheck: Equatable, Sendable {
    case derived(code: String)
    case failed(ConfirmationCodeError)

    static let sampleHandshakeHash = Data(repeating: 0, count: 32)

    public init(using codes: some ConfirmationCodeDeriving) {
        do {
            self = .derived(code: try codes.confirmationCode(handshakeHash: Self.sampleHandshakeHash))
        } catch {
            Logger.crypto.error("Confirmation code binding failed: \(String(describing: error), privacy: .public)")
            self = .failed(error)
        }
    }
}
