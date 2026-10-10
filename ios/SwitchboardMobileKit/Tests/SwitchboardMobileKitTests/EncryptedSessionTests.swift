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
        let records = try connection.phone.seal(largeEnvelope)
        #expect(records.count > 1)
        var opened: Data?
        for record in records {
            opened = try connection.mac.open(record)
        }
        #expect(opened == largeEnvelope)
        #expect(try deliver(Data("reply".utf8), from: connection.mac, to: connection.phone) == Data("reply".utf8))
    }

    /// Only a record that fails to decrypt breaks the stream; a frame of
    /// another type after the first open leaves the session usable.
    @Test func aFrameOfAnotherTypeAfterTheFirstOpenLeavesTheSessionOpen() throws {
        let connection = try connect()
        #expect(try deliver(Data("hello".utf8), from: connection.mac, to: connection.phone) == Data("hello".utf8))
        #expect(throws: RemoteCryptoError.unexpectedFrame) { try connection.phone.open(Data([3, 0, 0])) }
        #expect(!connection.phone.isClosed)
        #expect(try deliver(Data("next".utf8), from: connection.mac, to: connection.phone) == Data("next".utf8))
    }

    /// The phone gave up on one attempt and started another; the Mac's late
    /// reply to the first leaves the second waiting for its own.
    @Test func aReplyToAnAbandonedAttemptIsIgnored() throws {
        let phoneKeys = try PhoneKeys.generate()
        let macKeys = try PhoneKeys.generate()
        let abandoned = try ConnectionHandshake(keys: phoneKeys, macNoisePublicKey: macKeys.noisePublicKey)
        let handshake = try ConnectionHandshake(keys: phoneKeys, macNoisePublicKey: macKeys.noisePublicKey)
        let (_, late) = try EncryptedSession.accept(
            macKeys: macKeys,
            phoneNoisePublicKey: phoneKeys.noisePublicKey,
            message1: abandoned.message1
        )
        let (mac, message2) = try EncryptedSession.accept(
            macKeys: macKeys,
            phoneNoisePublicKey: phoneKeys.noisePublicKey,
            message1: handshake.message1
        )
        #expect(throws: RemoteCryptoError.unexpectedFrame) { try handshake.finish(message2: late) }
        let phone = try handshake.finish(message2: message2)
        #expect(try deliver(Data("hello".utf8), from: mac, to: phone) == Data("hello".utf8))
    }

    /// The phone's message 1 is exactly the key the Mac echoes; sent back to
    /// the phone, its frame type refuses it, and the pairing completes.
    @Test func thePhonesOwnPairingMessageSentBackIsIgnored() throws {
        let phoneKeys = try PhoneKeys.generate()
        let macKeys = try PhoneKeys.generate()
        let preSharedKey = Data(repeating: 7, count: 32)
        let phone = try PhonePairing(keys: phoneKeys, macNoisePublicKey: macKeys.noisePublicKey, preSharedKey: preSharedKey)
        let mac = try MacPairing(macKeys: macKeys, preSharedKey: preSharedKey, macName: "Studio Mac", message1: phone.message1)
        #expect(throws: RemoteCryptoError.unexpectedFrame) { try phone.respond(phoneName: "Jo's iPhone", message2: phone.message1) }
        let response = try phone.respond(phoneName: "Jo's iPhone", message2: mac.message2)
        #expect(try mac.finish(message3: response.message3).confirmationCode == response.confirmationCode)
    }

    @Test func aPairingReplyToAnAbandonedAttemptIsIgnored() throws {
        let phoneKeys = try PhoneKeys.generate()
        let macKeys = try PhoneKeys.generate()
        let preSharedKey = Data(repeating: 7, count: 32)
        let abandoned = try PhonePairing(keys: phoneKeys, macNoisePublicKey: macKeys.noisePublicKey, preSharedKey: preSharedKey)
        let phone = try PhonePairing(keys: phoneKeys, macNoisePublicKey: macKeys.noisePublicKey, preSharedKey: preSharedKey)
        let late = try MacPairing(macKeys: macKeys, preSharedKey: preSharedKey, macName: "Studio Mac", message1: abandoned.message1)
        let mac = try MacPairing(macKeys: macKeys, preSharedKey: preSharedKey, macName: "Studio Mac", message1: phone.message1)
        #expect(throws: RemoteCryptoError.unexpectedFrame) { try phone.respond(phoneName: "Jo's iPhone", message2: late.message2) }
        let response = try phone.respond(phoneName: "Jo's iPhone", message2: mac.message2)
        #expect(try mac.finish(message3: response.message3).confirmationCode == response.confirmationCode)
    }

    /// A record from the Mac's previous connection names that session, so it
    /// is ignored and the new session carries on.
    @Test func aRecordFromAnEarlierConnectionIsIgnored() throws {
        let old = try connect()
        let connection = try connect()
        let stale = try #require(try old.mac.seal(Data("old".utf8)).first)
        #expect(throws: RemoteCryptoError.staleRecord) { try connection.phone.open(stale) }
        #expect(!connection.phone.isClosed)
        #expect(!connection.phone.hasOpenedRecord)
        #expect(try deliver(Data("hello".utf8), from: connection.mac, to: connection.phone) == Data("hello".utf8))
    }

    /// A damaged first record names this session, so it breaks the stream at
    /// once rather than waiting out the deadline.
    @Test func aDamagedFirstRecordBreaksTheStream() throws {
        let connection = try connect()
        var record = try #require(try connection.mac.seal(Data("hello".utf8)).first)
        record[record.count - 1] ^= 1
        #expect(throws: RemoteCryptoError.streamBroken) { try connection.phone.open(record) }
        #expect(connection.phone.isClosed)
    }

    /// Longer than any frame: refused before it is copied into Rust, and the
    /// session carries on.
    @Test func anOversizeFrameIsRefusedWithoutTouchingTheSession() throws {
        let connection = try connect()
        #expect(throws: RemoteCryptoError.unexpectedFrame) {
            try connection.phone.open(Data([4]) + Data(count: 70_000))
        }
        #expect(!connection.phone.isClosed)
        #expect(try deliver(Data("hello".utf8), from: connection.mac, to: connection.phone) == Data("hello".utf8))
    }

    /// A record lost in transit leaves the next a nonce ahead, so it does not
    /// decrypt: the session closes rather than waiting forever on the gap.
    @Test func aLostRecordBreaksTheStream() throws {
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
        let preSharedKey = Data(repeating: 7, count: 32)
        let pairing = try PhonePairing(keys: phoneKeys, macNoisePublicKey: macKeys.noisePublicKey, preSharedKey: preSharedKey)
        let mac = try MacPairing(macKeys: macKeys, preSharedKey: preSharedKey, macName: "Mac", message1: pairing.message1)
        #expect(try FrameType(of: pairing.message1) == .pairing)
        #expect(try FrameType(of: mac.message2) == .pairingReply)
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

    /// A spec table on purpose: which errors are terminal decides when the
    /// phone reconnects, so changing it in Rust should take an edit here too.
    @Test func onlyErrorsThatUseUpTheirObjectAreTerminal() {
        let terminal: [RemoteCryptoError] = [
            .handshakeFailed, .unexpectedPeerKey, .invalidPayload, .streamBroken, .protocolViolation, .sendFailed,
            .sessionClosed, .bindingFailure,
        ]
        let nonTerminal: [RemoteCryptoError] = [
            .invalidKeyBlob, .invalidKey, .randomnessUnavailable, .messageTooLarge(length: 1), .unexpectedFrame,
            .staleRecord,
        ]
        for error in terminal {
            #expect(error.isTerminal, "\(error)")
        }
        for error in nonTerminal {
            #expect(!error.isTerminal, "\(error)")
        }
        for error in terminal + nonTerminal {
            #expect(error.roundTripsThroughRust, "\(error)")
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
