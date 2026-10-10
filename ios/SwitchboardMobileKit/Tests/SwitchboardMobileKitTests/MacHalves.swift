import Foundation
internal import SwitchboardRemoteCrypto
@testable import SwitchboardMobileKit

// The Mac's halves of the handshakes, so tests can run real round trips
// through the same handshake and record encryption the Mac runs (not its
// routing, which is tested in Rust). They live here, not in the package,
// because the app never plays the Mac; `make check-ios` fails if anything
// outside the tests names the generated class and function they call. This is
// the only test file that may import the bindings.

extension EncryptedSession {
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
}

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
