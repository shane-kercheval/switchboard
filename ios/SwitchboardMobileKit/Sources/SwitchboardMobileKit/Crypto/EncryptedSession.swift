import Foundation
internal import SwitchboardRemoteCrypto

/// The phone's side of opening a connection to its paired Mac. A reference
/// type because it is single-use: its first real reply uses it up.
public final class ConnectionHandshake: Sendable {
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
///
/// Until a record opens, a record that does not decrypt (`recordRejected`) is
/// a stale one from the Mac's previous session and is ignored; the transport
/// gives up on a session that opens nothing within its deadline. After the
/// first record opens, one that does not decrypt closes the session
/// (`streamBroken`), and the transport reconnects.
public final class EncryptedSession: Sendable {
    private let session: Session

    init(session: Session) {
        self.session = session
    }

    /// The Mac's half of a connection, for tests only: it lets them run a real
    /// round trip through the same code the Mac runs. The app never plays the
    /// Mac. `internal` only keeps it out of the public API, so keep it that
    /// way and never call it from production code.
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

    /// Whether any record has decrypted yet. The transport ends a session that
    /// has opened none within its deadline.
    public var hasOpenedRecord: Bool { session.hasOpenedRecord() }

    public var isClosed: Bool { session.isClosed() }
}
