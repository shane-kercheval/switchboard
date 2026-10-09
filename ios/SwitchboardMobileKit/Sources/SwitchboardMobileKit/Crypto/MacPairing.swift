import Foundation
internal import SwitchboardRemoteCrypto

/// The Mac's half of pairing, for tests only: it lets them run a real pairing
/// round trip through the same code the Mac runs. The app never plays the Mac.
/// `make check-ios` fails if anything outside the tests constructs one.
final class MacPairing: Sendable {
    private let handshake: MacPairingHandshake

    struct Candidate: Equatable, Sendable {
        let phoneNoisePublicKey: Data
        let phoneIdentityPublicKey: Data
        let phoneName: String
        let confirmationCode: String
    }

    init(macKeys: PhoneKeys, preSharedKey: Data, macName: String, message1: Data) throws(RemoteCryptoError) {
        handshake = try bridging {
            try MacPairingHandshake(keys: macKeys.keys, psk: preSharedKey, macName: macName, message1: message1)
        }
    }

    var message2: Data { handshake.message2() }

    func finish(message3: Data) throws(RemoteCryptoError) -> Candidate {
        let candidate = try bridging { try handshake.finish(message3: message3) }
        return Candidate(
            phoneNoisePublicKey: candidate.phoneNoisePublicKey,
            phoneIdentityPublicKey: candidate.phoneIdentityPublicKey,
            phoneName: candidate.phoneName,
            confirmationCode: candidate.confirmationCode
        )
    }
}
