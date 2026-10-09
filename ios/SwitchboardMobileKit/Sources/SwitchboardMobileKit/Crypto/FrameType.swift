import Foundation
internal import SwitchboardRemoteCrypto

/// What a frame is, so the transport routes it — to pairing, the connection
/// handshake, or the session — without touching any of them. Only the first
/// byte crosses into Rust; the step that consumes the frame checks it in full.
public enum FrameType: Equatable, Sendable {
    /// The phone's own pairing messages, 1 and 3.
    case pairing
    /// The Mac's pairing message 2.
    case pairingReply
    case sessionRequest
    case sessionReply
    case record

    public init(of frame: Data) throws(RemoteCryptoError) {
        guard let tag = frame.first else { throw .unexpectedFrame }
        switch try bridging({ try frameKind(tag: tag) }) {
        case .pairing: self = .pairing
        case .pairingReply: self = .pairingReply
        case .sessionRequest: self = .sessionRequest
        case .sessionReply: self = .sessionReply
        case .record: self = .record
        }
    }

    /// Refuses a frame longer than any step accepts before it is copied into
    /// Rust, which checks the length again in full.
    static func checkLength(of frame: Data) throws(RemoteCryptoError) {
        guard frame.count <= maxLength else { throw .unexpectedFrame }
    }

    private static let maxLength = Int(maxFrameLen())
}
