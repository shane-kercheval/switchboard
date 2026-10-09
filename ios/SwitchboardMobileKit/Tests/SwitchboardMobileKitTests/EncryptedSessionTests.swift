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

    /// Before any record opens, an undecryptable one may be a stale record from
    /// the Mac's previous session, so it is ignored.
    @Test func anUndecryptableRecordBeforeTheFirstOpenIsIgnored() throws {
        let connection = try connect()
        var record = try #require(try connection.mac.seal(Data("hello".utf8)).first)
        record[record.count - 1] ^= 1
        #expect(throws: RemoteCryptoError.recordRejected) { try connection.phone.open(record) }
        #expect(!connection.phone.isClosed)
        #expect(!connection.phone.hasOpenedRecord)
    }

    /// After a record opens, an undecryptable one means the stream is broken:
    /// the session closes rather than waiting forever on a missing record.
    @Test func anUndecryptableRecordAfterTheFirstOpenBreaksTheStream() throws {
        let connection = try connect()
        #expect(try deliver(Data("one".utf8), from: connection.mac, to: connection.phone) == Data("one".utf8))
        #expect(connection.phone.hasOpenedRecord)
        _ = try connection.mac.seal(Data("lost".utf8))
        let next = try #require(try connection.mac.seal(Data("three".utf8)).first)
        #expect(throws: RemoteCryptoError.streamBroken) { try connection.phone.open(next) }
        #expect(connection.phone.isClosed)
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
        #expect(throws: RemoteCryptoError.unexpectedFrame) { try FrameType(of: Data([9, 1, 2])) }
    }

    @Test func storedKeysRestoreToTheSameDevice() throws {
        let keys = try PhoneKeys.generate()
        let restored = try PhoneKeys(restoringFrom: keys.storageBytes)
        #expect(restored.deviceID == keys.deviceID)
        #expect(restored.noisePublicKey == keys.noisePublicKey)
        #expect(restored.identityPublicKey == keys.identityPublicKey)
        #expect(keys.identityPublicKey.count == 32)
        #expect(keys.identityPublicKey != keys.noisePublicKey)
        #expect(keys.deviceID.count == 26)
    }

    @Test func relayChallengeSignaturesAreDeterministicPerChallenge() throws {
        let keys = try PhoneKeys.generate()
        let signature = keys.signRelayChallenge(Data("challenge".utf8))
        #expect(signature.count == 64)
        #expect(keys.signRelayChallenge(Data("challenge".utf8)) == signature)
        #expect(keys.signRelayChallenge(Data("another".utf8)) != signature)
    }

    @Test func aMalformedKeyBlobIsRefused() {
        #expect(throws: RemoteCryptoError.invalidKeyBlob) { try PhoneKeys(restoringFrom: Data([2, 0, 0])) }
    }

    /// Both halves through the binding, so a field mapped to the wrong place
    /// on either side — two of them are `Data` — fails here, not at the first
    /// real pairing.
    @Test func pairingRoundTripsAndBothSidesShowTheSameCode() throws {
        let phoneKeys = try PhoneKeys.generate()
        let macKeys = try PhoneKeys.generate()
        let preSharedKey = Data(repeating: 7, count: 32)
        let phone = try PhonePairing(keys: phoneKeys, macNoisePublicKey: macKeys.noisePublicKey, preSharedKey: preSharedKey)
        let mac = try MacPairing(macKeys: macKeys, preSharedKey: preSharedKey, macName: "Studio Mac", message1: phone.message1)
        let response = try phone.respond(phoneName: "Jo's iPhone", message2: mac.message2)
        let candidate = try mac.finish(message3: response.message3)

        #expect(response.macNoisePublicKey == macKeys.noisePublicKey)
        #expect(response.macName == "Studio Mac")
        #expect(candidate.phoneNoisePublicKey == phoneKeys.noisePublicKey)
        #expect(candidate.phoneIdentityPublicKey == phoneKeys.identityPublicKey)
        #expect(candidate.phoneName == "Jo's iPhone")
        #expect(candidate.confirmationCode == response.confirmationCode)
        #expect(response.confirmationCode.count == 6)
    }

    @Test func onlyErrorsThatUseUpTheirObjectAreTerminal() {
        let terminal: [RemoteCryptoError] = [
            .handshakeFailed, .unexpectedPeerKey, .invalidPayload, .streamBroken, .protocolViolation, .sendFailed,
            .sessionClosed, .bindingFailure,
        ]
        let nonTerminal: [RemoteCryptoError] = [
            .invalidKeyBlob, .invalidKey, .randomnessUnavailable, .messageTooLarge(length: 1), .unexpectedFrame,
            .recordRejected,
        ]
        for error in terminal {
            #expect(error.isTerminal, "\(error)")
        }
        for error in nonTerminal {
            #expect(!error.isTerminal, "\(error)")
        }
    }

    /// A damaged reply uses the handshake up, so the error says so and the
    /// real reply arriving afterwards cannot revive it.
    @Test func aDamagedConnectionReplyIsTerminal() throws {
        let phoneKeys = try PhoneKeys.generate()
        let macKeys = try PhoneKeys.generate()
        let handshake = try ConnectionHandshake(keys: phoneKeys, macNoisePublicKey: macKeys.noisePublicKey)
        let (_, message2) = try EncryptedSession.accept(
            macKeys: macKeys,
            phoneNoisePublicKey: phoneKeys.noisePublicKey,
            message1: handshake.message1
        )
        var damaged = message2
        damaged[damaged.count - 1] ^= 1
        let error = #expect(throws: RemoteCryptoError.self) { try handshake.finish(message2: damaged) }
        #expect(error == .handshakeFailed)
        #expect(error?.isTerminal == true)
        #expect(throws: RemoteCryptoError.handshakeFailed) { try handshake.finish(message2: message2) }
    }

    @Test func aDamagedPairingReplyIsTerminal() throws {
        let phoneKeys = try PhoneKeys.generate()
        let macKeys = try PhoneKeys.generate()
        let preSharedKey = Data(repeating: 7, count: 32)
        let phone = try PhonePairing(keys: phoneKeys, macNoisePublicKey: macKeys.noisePublicKey, preSharedKey: preSharedKey)
        let mac = try MacPairing(macKeys: macKeys, preSharedKey: preSharedKey, macName: "Studio Mac", message1: phone.message1)
        var damaged = mac.message2
        damaged[damaged.count - 1] ^= 1
        let error = #expect(throws: RemoteCryptoError.self) { try phone.respond(phoneName: "Jo's iPhone", message2: damaged) }
        #expect(error == .handshakeFailed)
        #expect(error?.isTerminal == true)
        #expect(throws: RemoteCryptoError.handshakeFailed) {
            try phone.respond(phoneName: "Jo's iPhone", message2: mac.message2)
        }
    }

    @Test func pairingRefusesKeysOfTheWrongLength() throws {
        let keys = try PhoneKeys.generate()
        #expect(throws: RemoteCryptoError.invalidKey) {
            try PhonePairing(keys: keys, macNoisePublicKey: Data(count: 31), preSharedKey: Data(count: 32))
        }
    }
}
