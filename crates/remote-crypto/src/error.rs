use crate::HANDSHAKE_HASH_LEN;

/// Every failure crossing the crate's boundary. Handshake and record failures
/// carry no detail on purpose: what went wrong with an attacker's bytes is not
/// the caller's business, and the response to all of them is the same. Grows
/// as the crate does, so it is `#[non_exhaustive]`: a consumer outside this
/// crate keeps a catch-all arm, and a new variant breaks no build.
#[derive(Debug, PartialEq, Eq, thiserror::Error, uniffi::Error)]
#[non_exhaustive]
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
    /// A frame of the wrong type, of no known type, or longer than any Noise
    /// message. Refused before any handshake or session state is touched, so
    /// an open session stays open.
    #[error("the frame is not of the expected type")]
    UnexpectedFrame,
    /// The record did not decrypt under this session's keys. The session is
    /// closed. The record may belong to another session of the same device.
    #[error("the record did not authenticate, so the session is closed")]
    RecordRejected,
    /// The record decrypted, so the peer holds this session's keys, but its
    /// contents broke the fragment rules. The session is closed. The record
    /// belongs to no other session.
    #[error("the peer sent a malformed record, so the session is closed")]
    ProtocolViolation,
    /// A call on a session that an earlier failure already closed.
    #[error("the session is closed")]
    SessionClosed,
}
