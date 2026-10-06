import Foundation

/// Derives the six-digit code both screens show while pairing.
public protocol ConfirmationCodeDeriving: Sendable {
    func confirmationCode(handshakeHash: Data) throws(ConfirmationCodeError) -> String
}

public enum ConfirmationCodeError: Error, Equatable, Sendable {
    case invalidHandshakeHash(length: Int)
    /// The Rust library failed in a way its API does not describe, such as a
    /// panic or a binding built from a different version of the crate.
    case bindingFailure
}
