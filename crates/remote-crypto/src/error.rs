use crate::HANDSHAKE_HASH_LEN;

#[derive(Debug, PartialEq, Eq, thiserror::Error, uniffi::Error)]
pub enum CryptoError {
    #[error("handshake hash must be {HANDSHAKE_HASH_LEN} bytes, got {length}")]
    InvalidHandshakeHash { length: u64 },
    #[error("signature does not verify")]
    InvalidSignature,
}
