import Foundation
internal import SwitchboardRemoteCrypto

/// What a frame is, so the transport routes it — to pairing, the connection
/// handshake, or the session — without touching any of them. Only the first
/// byte crosses into Rust; the step that consumes the frame checks it in full.
public enum FrameType: Equatable, Sendable {
    case pairing
    case sessionRequest
    case sessionReply
    case record

    public init(of frame: Data) throws(RemoteCryptoError) {
        guard let tag = frame.first else { throw .unexpectedFrame }
        switch try bridging({ try frameKind(tag: tag) }) {
        case .pairing: self = .pairing
        case .sessionRequest: self = .sessionRequest
        case .sessionReply: self = .sessionReply
        case .record: self = .record
        }
    }
}
