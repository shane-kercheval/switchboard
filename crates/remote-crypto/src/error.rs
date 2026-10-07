use crate::HANDSHAKE_HASH_LEN;

/// Every failure crossing the crate's boundary. Handshake and record failures
/// carry no detail on purpose: what went wrong with an attacker's bytes is not
/// the caller's business, and the response to all of them is the same.
#[derive(Debug, PartialEq, Eq, thiserror::Error, uniffi::Error)]
pub enum CryptoError {
    #[error("handshake hash must be {HANDSHAKE_HASH_LEN} bytes, got {length}")]
    InvalidHandshakeHash { length: u64 },
    #[error("signature does not verify")]
    InvalidSignature,
    #[error("stored keys are malformed or from an unknown version")]
    InvalidKeyBlob,
    #[error("the operating system's random number generator failed")]
    RandomnessUnavailable,
    #[error("a key has the wrong length")]
    InvalidKey,
    #[error("the handshake failed")]
    HandshakeFailed,
    #[error("the peer presented a different key than the one expected")]
    UnexpectedPeerKey,
    #[error("a handshake payload is malformed or from an unknown version")]
    InvalidPayload,
    #[error("message of {length} bytes is too large to send")]
    MessageTooLarge { length: u64 },
    #[error("a record could not be opened, so the session is closed")]
    SessionClosed,
}
