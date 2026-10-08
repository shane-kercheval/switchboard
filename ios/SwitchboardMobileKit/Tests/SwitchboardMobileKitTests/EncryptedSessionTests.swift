import Foundation
import Testing
@testable import SwitchboardMobileKit

/// The binding end to end on the device: the phone's handshake and session
/// against the Mac's half, through the same Rust the Mac runs.
struct EncryptedSessionTests {
    struct Connection {
        let phone: EncryptedSession
        let mac: EncryptedSession
    }

    func connect() throws -> Connection {
        let phoneKeys = try PhoneKeys.generate()
        let macKeys = try PhoneKeys.generate()
        let handshake = try ConnectionHandshake(keys: phoneKeys, macNoisePublicKey: macKeys.noisePublicKey)
        let (mac, message2) = try EncryptedSession.accept(
            macKeys: macKeys,
            phoneNoisePublicKey: phoneKeys.noisePublicKey,
            message1: handshake.message1
        )
        return Connection(phone: try handshake.finish(message2: message2), mac: mac)
    }

    func deliver(_ envelope: Data, from sender: EncryptedSession, to receiver: EncryptedSession) throws -> Data? {
        var opened: Data?
        for record in try sender.seal(envelope) {
            opened = try receiver.open(record)
        }
        return opened
    }

    /// Larger than one record, so it also exercises fragmentation.
    let largeEnvelope = Data((0..<200_000).map { UInt8(truncatingIfNeeded: $0 % 251) })

    @Test func envelopesRoundTripInBothDirections() throws {
        let connection = try connect()
        #expect(try connection.phone.seal(largeEnvelope).count > 1)
        let connection2 = try connect()
        #expect(try deliver(largeEnvelope, from: connection2.phone, to: connection2.mac) == largeEnvelope)
        #expect(try deliver(Data("reply".utf8), from: connection2.mac, to: connection2.phone) == Data("reply".utf8))
    }

    @Test func aTamperedRecordIsRejectedWithoutClosingTheSession() throws {
        let connection = try connect()
        var record = try #require(try connection.phone.seal(Data("hello".utf8)).first)
        record[record.count - 1] ^= 1
        #expect(throws: RemoteCryptoError.recordRejected) { try connection.mac.open(record) }
        #expect(!connection.mac.isClosed)
    }

    @Test func aReplyOfTheWrongTypeLeavesTheHandshakeWaiting() throws {
        let phoneKeys = try PhoneKeys.generate()
        let macKeys = try PhoneKeys.generate()
        let handshake = try ConnectionHandshake(keys: phoneKeys, macNoisePublicKey: macKeys.noisePublicKey)
        #expect(throws: RemoteCryptoError.unexpectedFrame) { try handshake.finish(message2: handshake.message1) }
        let (_, message2) = try EncryptedSession.accept(
            macKeys: macKeys,
            phoneNoisePublicKey: phoneKeys.noisePublicKey,
            message1: handshake.message1
        )
        _ = try handshake.finish(message2: message2)
        #expect(throws: RemoteCryptoError.handshakeFailed) { try handshake.finish(message2: message2) }
    }

    @Test func framesAreClassifiedByType() throws {
        let phoneKeys = try PhoneKeys.generate()
        let macKeys = try PhoneKeys.generate()
        let handshake = try ConnectionHandshake(keys: phoneKeys, macNoisePublicKey: macKeys.noisePublicKey)
        #expect(try FrameType(of: handshake.message1) == .sessionRequest)
        #expect(throws: RemoteCryptoError.unexpectedFrame) { try FrameType(of: Data()) }
    }

    @Test func storedKeysRestoreToTheSameDevice() throws {
        let keys = try PhoneKeys.generate()
        let restored = try PhoneKeys(restoringFrom: keys.storageBytes)
        #expect(restored.deviceID == keys.deviceID)
        #expect(restored.noisePublicKey == keys.noisePublicKey)
        #expect(keys.deviceID.count == 26)
    }

    @Test func aMalformedKeyBlobIsRefused() {
        #expect(throws: RemoteCryptoError.invalidKeyBlob) { try PhoneKeys(restoringFrom: Data([2, 0, 0])) }
    }

    @Test func pairingRefusesKeysOfTheWrongLength() throws {
        let keys = try PhoneKeys.generate()
        #expect(throws: RemoteCryptoError.invalidKey) {
            try PhonePairing(keys: keys, macNoisePublicKey: Data(count: 31), preSharedKey: Data(count: 32))
        }
    }
}
