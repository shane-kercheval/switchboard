use crate::HANDSHAKE_HASH_LEN;

/// Grows as the crate does, so it is `#[non_exhaustive]`: a consumer outside
/// this crate keeps a catch-all arm, and a new variant breaks no build.
#[derive(Debug, PartialEq, Eq, thiserror::Error, uniffi::Error)]
#[non_exhaustive]
pub enum CryptoError {
    #[error("handshake hash must be {HANDSHAKE_HASH_LEN} bytes, got {length}")]
    InvalidHandshakeHash { length: u64 },
    #[error("signature does not verify")]
    InvalidSignature,
}
