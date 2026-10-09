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

    /// Reads the Mac's reply. A frame of another type, or a reply to an
    /// attempt this handshake replaced, throws `unexpectedFrame` and leaves it
    /// waiting; its own reply uses it up.
    public func finish(message2: Data) throws(RemoteCryptoError) -> EncryptedSession {
        EncryptedSession(session: try bridging { try handshake.finish(message2: message2) })
    }
}

/// An established, encrypted connection. Safe to call from any thread, in that
/// concurrent calls never corrupt its state, and one `seal` produces all of an
/// envelope's records at once. That does not make concurrent sending safe:
/// each record is numbered as it is sealed, and the Mac drops the connection
/// at a gap, so records must be transmitted in the order they were sealed,
/// across envelopes as well as within one. Seal and send from one place — a
/// single queue with one consumer — never from several threads.
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
