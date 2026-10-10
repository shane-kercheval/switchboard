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
    /// A frame of the wrong type, or oversize, or a handshake reply that
    /// answers another attempt. Nothing was touched: a waiting handshake keeps
    /// waiting.
    case unexpectedFrame
    /// A record for another session: a leftover from the Mac's previous
    /// connection. Nothing changed; ignore it.
    case staleRecord
    /// A record decrypted but was malformed. The session is closed.
    case protocolViolation
    case sessionClosed
    case sendFailed
    /// A record for this session did not decrypt: the stream from the Mac is
    /// broken. The session is closed; reconnect.
    case streamBroken
    /// The Rust library failed in a way its API does not describe, such as a
    /// panic or a binding built from a different version of the crate.
    case bindingFailure

    /// Whether the handshake or session this error came from can't be used
    /// again. The caller decides what follows: the transport starts a new
    /// handshake, with backoff, on the same relay connection; pairing shows
    /// the attempt as failed. Rust decides, since it is Rust that uses a
    /// handshake up or closes a session; a binding failure is terminal.
    public var isTerminal: Bool {
        rustError.map { cryptoErrorIsTerminal(error: $0) } ?? true
    }

    /// Whether this case maps to its Rust error and back to itself, so the
    /// two tables below agree. The tests call it, since only `Crypto/` may
    /// name the Rust errors.
    var roundTripsThroughRust: Bool {
        rustError.map { RemoteCryptoError(rustError: $0) == self } ?? true
    }

    /// The Rust error this case stands for, `nil` for `bindingFailure`.
    private var rustError: CryptoError? {
        switch self {
        case .invalidKeyBlob: .InvalidKeyBlob
        case .invalidKey: .InvalidKey
        case .randomnessUnavailable: .RandomnessUnavailable
        case .handshakeFailed: .HandshakeFailed
        case .unexpectedPeerKey: .UnexpectedPeerKey
        case .invalidPayload: .InvalidPayload
        case .messageTooLarge(let length): .MessageTooLarge(length: UInt64(clamping: length))
        case .unexpectedFrame: .UnexpectedFrame
        case .staleRecord: .StaleRecord
        case .protocolViolation: .ProtocolViolation
        case .sessionClosed: .SessionClosed
        case .sendFailed: .SendFailed
        case .streamBroken: .StreamBroken
        case .bindingFailure: nil
        }
    }

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
        case CryptoError.StaleRecord: self = .staleRecord
        case CryptoError.ProtocolViolation: self = .protocolViolation
        case CryptoError.SessionClosed: self = .sessionClosed
        case CryptoError.SendFailed: self = .sendFailed
        case CryptoError.StreamBroken: self = .streamBroken
        // Not library failures, so not logged as one: the Mac's side, which
        // the app never runs but the tests do (`InvalidSignature`,
        // `NotConnected`), and `confirmationCode`'s own error.
        case CryptoError.InvalidSignature, CryptoError.NotConnected, CryptoError.InvalidHandshakeHash:
            self = .bindingFailure
        default:
            logUnexpectedRustError(error)
            self = .bindingFailure
        }
    }
}

/// For an error the Rust library's API does not describe.
func logUnexpectedRustError(_ error: any Error) {
    Logger.crypto.error("Unexpected error from the Rust library: \(String(describing: error), privacy: .private)")
}

/// Runs a binding call, mapping its error.
func bridging<T>(_ body: () throws -> T) throws(RemoteCryptoError) -> T {
    do {
        return try body()
    } catch {
        throw RemoteCryptoError(rustError: error)
    }
}
