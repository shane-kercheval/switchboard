import Foundation
internal import SwitchboardRemoteCrypto

/// The phone's side of opening a connection to its paired Mac.
public struct ConnectionHandshake: Sendable {
    private let handshake: SessionHandshake

    public init(keys: PhoneKeys, macNoisePublicKey: Data) throws(RemoteCryptoError) {
        handshake = try bridging { try SessionHandshake(keys: keys.keys, macNoisePublicKey: macNoisePublicKey) }
    }

    public var message1: Data { handshake.message1() }

    /// Reads the Mac's reply. A frame of another type throws `unexpectedFrame`
    /// and leaves the handshake waiting; a real reply uses it up.
    public func finish(message2: Data) throws(RemoteCryptoError) -> EncryptedSession {
        EncryptedSession(session: try bridging { try handshake.finish(message2: message2) })
    }
}

/// An established, encrypted connection. Safe to call from any thread: one
/// `seal` produces all of an envelope's records at once. The caller must still
/// transmit them in the order returned and finish one envelope's records
/// before the next's.
public struct EncryptedSession: Sendable {
    private let session: Session

    init(session: Session) {
        self.session = session
    }

    /// The Mac's half of a connection. The app never plays the Mac; this lets
    /// tests run a real round trip through the same code the Mac runs.
    static func accept(
        macKeys: PhoneKeys,
        phoneNoisePublicKey: Data,
        message1: Data
    ) throws(RemoteCryptoError) -> (session: EncryptedSession, message2: Data) {
        let acceptance = try bridging {
            try acceptSession(keys: macKeys.keys, phoneNoisePublicKey: phoneNoisePublicKey, message1: message1)
        }
        return (EncryptedSession(session: acceptance.session), acceptance.message2)
    }

    public func seal(_ envelope: Data) throws(RemoteCryptoError) -> [Data] {
        try bridging { try session.seal(envelope: envelope) }
    }

    /// The envelope once its last record arrives, `nil` before then.
    public func open(_ record: Data) throws(RemoteCryptoError) -> Data? {
        try bridging { try session.open(record: record) }
    }

    public var isClosed: Bool { session.isClosed() }
}

/// What a frame is, so it can be routed without touching any session.
public enum FrameType: Equatable, Sendable {
    case pairing
    case sessionRequest
    case sessionReply
    case record

    public init(of frame: Data) throws(RemoteCryptoError) {
        switch try bridging({ try frameKind(frame: frame) }) {
        case .pairing: self = .pairing
        case .sessionRequest: self = .sessionRequest
        case .sessionReply: self = .sessionReply
        case .record: self = .record
        }
    }
}
