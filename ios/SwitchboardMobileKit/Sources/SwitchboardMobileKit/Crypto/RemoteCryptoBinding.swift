import Foundation
internal import SwitchboardRemoteCrypto

/// The Rust implementation from `crates/remote-crypto`, the same code the Mac
/// runs. Generated UniFFI types stop here; callers see only this package's
/// own protocols and errors.
public struct RemoteCryptoBinding: ConfirmationCodeDeriving {
    public init() {}

    public func confirmationCode(handshakeHash: Data) throws(ConfirmationCodeError) -> String {
        do {
            return try SwitchboardRemoteCrypto.confirmationCode(handshakeHash: handshakeHash)
        } catch CryptoError.InvalidHandshakeHash(let length) {
            throw .invalidHandshakeHash(length: Int(clamping: length))
        } catch {
            logUnexpectedRustError(error)
            throw .bindingFailure
        }
    }
}
