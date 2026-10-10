import Foundation
internal import SwitchboardRemoteCrypto

/// This phone's long-term keys, held by the shared Rust library. No private
/// key is ever visible here: only the opaque storage blob, which belongs in
/// the Keychain and nowhere else.
public struct PhoneKeys: Sendable {
    let keys: DeviceKeys

    /// Fresh keys from the operating system's random number generator.
    public static func generate() throws(RemoteCryptoError) -> PhoneKeys {
        PhoneKeys(keys: try bridging { try DeviceKeys.generate() })
    }

    /// Keys from `storageBytes`, as read back from the Keychain.
    public init(restoringFrom storageBytes: Data) throws(RemoteCryptoError) {
        keys = try bridging { try DeviceKeys.restore(blob: storageBytes) }
    }

    private init(keys: DeviceKeys) {
        self.keys = keys
    }

    /// The blob to store in the Keychain. Opaque; never log or persist it
    /// anywhere else.
    public var storageBytes: Data { keys.storageBytes() }

    public var deviceID: String { keys.deviceId() }

    public var noisePublicKey: Data { keys.noisePublicKey() }

    /// The Ed25519 key the relay checks `deviceID` and challenge signatures
    /// against at registration.
    public var identityPublicKey: Data { keys.identityPublicKey() }

    /// Answers the relay's registration challenge.
    public func signRelayChallenge(_ challenge: Data) -> Data {
        keys.signRelayChallenge(challenge: challenge)
    }
}
