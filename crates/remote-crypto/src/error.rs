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
    /// message, or a handshake reply that answers another attempt. Refused
    /// before any handshake or session state is touched, so an open session
    /// stays open and a waiting handshake keeps waiting.
    #[error("the frame is not of the expected type")]
    UnexpectedFrame,
    /// The record named this session but did not decrypt under its keys.
    /// Nothing changed in the session; `DeviceSessions` and `PhoneSession`
    /// turn this into `StreamBroken`, since a session's records arrive in
    /// order and one that does not decrypt means the stream has a gap or a
    /// forgery.
    #[error("the record did not authenticate under this session's keys")]
    RecordRejected,
    /// The record names a session other than this one: a leftover from a
    /// connection that has since been replaced, or one that never existed.
    /// Nothing changed; the caller ignores it.
    #[error("the record belongs to another session")]
    StaleRecord,
    /// The record decrypted, so the peer holds this session's keys, but its
    /// contents broke the fragment rules. The session is closed. The record
    /// belongs to no other session.
    #[error("the peer sent a malformed record, so the session is closed")]
    ProtocolViolation,
    /// A call on a session that an earlier failure already closed.
    #[error("the session is closed")]
    SessionClosed,
    /// Encrypting a record failed, so the session is closed.
    #[error("a record could not be encrypted, so the session is closed")]
    SendFailed,
    /// The device has no session to open or seal with.
    #[error("the device is not connected")]
    NotConnected,
    /// A record named a live session but did not decrypt: that session's
    /// in-order stream has a gap or a forgery. The session is closed; the
    /// phone reconnects.
    #[error("a record for this session did not decrypt, so the session is closed")]
    StreamBroken,
}

impl CryptoError {
    /// Whether the handshake or session this error came from can't be used
    /// again: a handshake step that read its real message, or a session that
    /// closed. The caller decides what follows — a new handshake, a failed
    /// pairing. False for errors that leave their object usable, and for
    /// construction and input errors, which come before anything exists to
    /// end. `InvalidSignature` is terminal because its only handshake source,
    /// the Mac reading pairing message 3, has used up its handshake by then.
    pub fn is_terminal(&self) -> bool {
        match self {
            Self::InvalidSignature
            | Self::HandshakeFailed
            | Self::UnexpectedPeerKey
            | Self::InvalidPayload
            | Self::ProtocolViolation
            | Self::SessionClosed
            | Self::SendFailed
            | Self::StreamBroken => true,
            Self::InvalidHandshakeHash { .. }
            | Self::InvalidKeyBlob
            | Self::RandomnessUnavailable
            | Self::InvalidKey
            | Self::MessageTooLarge { .. }
            | Self::UnexpectedFrame
            | Self::RecordRejected
            | Self::StaleRecord
            | Self::NotConnected => false,
        }
    }
}
