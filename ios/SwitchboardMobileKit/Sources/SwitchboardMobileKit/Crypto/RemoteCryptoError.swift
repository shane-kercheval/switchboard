import Foundation
import os
internal import SwitchboardRemoteCrypto

/// What can go wrong in the phone's cryptography, as this package's callers
/// see it. Generated binding errors are mapped here and go no further.
public enum RemoteCryptoError: Error, Equatable, Sendable {
    /// Stored keys are malformed or from an unknown version.
    case invalidKeyBlob
    /// A key or pre-shared key has the wrong length.
    case invalidKey
    case randomnessUnavailable
    case handshakeFailed
    /// The Mac presented a key other than the one in its QR code.
    case unexpectedPeerKey
    case invalidPayload
    case messageTooLarge(length: Int)
    /// A frame of the wrong type, or oversize. Nothing was touched.
    case unexpectedFrame
    /// A record did not decrypt before the session had opened any: a stale
    /// record from the Mac's previous session. The session is unchanged.
    case recordRejected
    /// A record decrypted but was malformed. The session is closed.
    case protocolViolation
    case sessionClosed
    case sendFailed
    /// A record did not decrypt after the session had opened one: the stream
    /// from the Mac is broken. The session is closed; reconnect.
    case streamBroken
    /// The Rust library failed in a way its API does not describe, such as a
    /// panic or a binding built from a different version of the crate.
    case bindingFailure

    init(rustError error: any Error) {
        switch error {
        case CryptoError.InvalidKeyBlob: self = .invalidKeyBlob
        case CryptoError.InvalidKey: self = .invalidKey
        case CryptoError.RandomnessUnavailable: self = .randomnessUnavailable
        case CryptoError.HandshakeFailed: self = .handshakeFailed
        case CryptoError.UnexpectedPeerKey: self = .unexpectedPeerKey
        case CryptoError.InvalidPayload: self = .invalidPayload
        case CryptoError.MessageTooLarge(let length): self = .messageTooLarge(length: Int(clamping: length))
        case CryptoError.UnexpectedFrame: self = .unexpectedFrame
        case CryptoError.RecordRejected: self = .recordRejected
        case CryptoError.ProtocolViolation: self = .protocolViolation
        case CryptoError.SessionClosed: self = .sessionClosed
        case CryptoError.SendFailed: self = .sendFailed
        case CryptoError.StreamBroken: self = .streamBroken
        default:
            Logger.crypto.error("Unexpected error from the Rust library: \(String(describing: error), privacy: .private)")
            self = .bindingFailure
        }
    }
}

/// Runs a binding call, mapping its error.
func bridging<T>(_ body: () throws -> T) throws(RemoteCryptoError) -> T {
    do {
        return try body()
    } catch {
        throw RemoteCryptoError(rustError: error)
    }
}
