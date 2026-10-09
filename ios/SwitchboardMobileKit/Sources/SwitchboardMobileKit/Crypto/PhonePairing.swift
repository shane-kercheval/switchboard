import Foundation
internal import SwitchboardRemoteCrypto

/// The phone's side of pairing, from the QR code to message 3. A reference
/// type because it is single-use: its first real message 2 uses it up.
public final class PhonePairing: Sendable {
    private let handshake: PairingHandshake

    /// What the phone learns from the Mac's message 2, and the message 3 to
    /// send. Not trusted yet: keep the Mac's key only once the Mac reports that
    /// the user typed `confirmationCode`.
    public struct Response: Equatable, Sendable {
        public let macNoisePublicKey: Data
        public let macName: String
        public let confirmationCode: String
        public let message3: Data
    }

    /// Starts pairing with the Mac described by its QR code.
    public init(keys: PhoneKeys, macNoisePublicKey: Data, preSharedKey: Data) throws(RemoteCryptoError) {
        handshake = try bridging {
            try PairingHandshake(keys: keys.keys, macNoisePublicKey: macNoisePublicKey, psk: preSharedKey)
        }
    }

    public var message1: Data { handshake.message1() }

    /// Reads the Mac's message 2. A frame of another type throws
    /// `unexpectedFrame` and leaves the pairing waiting; a real message 2 uses
    /// it up, whatever the outcome.
    public func respond(phoneName: String, message2: Data) throws(RemoteCryptoError) -> Response {
        let response = try bridging { try handshake.respond(phoneName: phoneName, message2: message2) }
        return Response(
            macNoisePublicKey: response.macNoisePublicKey,
            macName: response.macName,
            confirmationCode: response.confirmationCode,
            message3: response.message3
        )
    }
}
