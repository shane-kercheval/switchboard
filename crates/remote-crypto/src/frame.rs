//! The one-byte type every frame this crate produces starts with, so each side
//! routes a frame to the right handshake step or session without guessing. A
//! connection request and a small record can be the same length, so without
//! the type a replayed request could be routed to a live session as a record.
//!
//! The byte is outside the encryption: a relay that changes it only makes the
//! frame fail, which it could do anyway. Relay-level fields, such as a pairing
//! token, travel in the relay's own frame, never in these bytes.

use crate::CryptoError;
use crate::fragment::MAX_RECORD_LEN;

/// Declares `FrameKind` and the list of every kind from one table, so a new
/// kind is encoded by `tagged` and decoded by `kind` without a second edit.
macro_rules! frame_kinds {
    ($($(#[$doc:meta])* $name:ident = $byte:literal,)+) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        #[repr(u8)]
        #[non_exhaustive]
        pub enum FrameKind {
            $($(#[$doc])* $name = $byte,)+
        }

        impl FrameKind {
            /// Every kind, in declaration order.
            pub const ALL: &[FrameKind] = &[$(FrameKind::$name,)+];
        }
    };
}

frame_kinds! {
    /// Any of the three pairing handshake messages.
    Pairing = 1,
    /// A phone opening a connection: `Noise_KK` message 1.
    SessionRequest = 2,
    /// The Mac's answer: `Noise_KK` message 2.
    SessionReply = 3,
    /// An encrypted fragment on an established session.
    Record = 4,
}

/// Classifies a frame without touching any handshake or session state. Fails
/// for an empty frame, an unknown type, or a frame longer than any Noise
/// message — the last is refused here, before anything is allocated for it.
pub fn kind(frame: &[u8]) -> Result<FrameKind, CryptoError> {
    if frame.len() > 1 + MAX_RECORD_LEN {
        return Err(CryptoError::UnexpectedFrame);
    }
    kind_of_tag(*frame.first().ok_or(CryptoError::UnexpectedFrame)?)
}

/// The kind a frame's first byte names. Only classifies: the step that
/// consumes the frame still checks it in full, length included.
pub fn kind_of_tag(tag: u8) -> Result<FrameKind, CryptoError> {
    FrameKind::ALL
        .iter()
        .copied()
        .find(|kind| *kind as u8 == tag)
        .ok_or(CryptoError::UnexpectedFrame)
}

/// The body of a frame of the expected kind.
pub(crate) fn body(expected: FrameKind, frame: &[u8]) -> Result<&[u8], CryptoError> {
    if kind(frame)? == expected {
        Ok(&frame[1..])
    } else {
        Err(CryptoError::UnexpectedFrame)
    }
}

pub(crate) fn tagged(kind: FrameKind, body: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(1 + body.len());
    frame.push(kind as u8);
    frame.extend_from_slice(body);
    frame
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_kind_round_trips() {
        for &kind in FrameKind::ALL {
            let frame = tagged(kind, b"body");
            assert_eq!(super::kind(&frame), Ok(kind));
            assert_eq!(body(kind, &frame), Ok(&b"body"[..]));
        }
    }

    #[test]
    fn a_frame_of_another_kind_is_refused() {
        let frame = tagged(FrameKind::SessionRequest, b"body");
        assert_eq!(
            body(FrameKind::Record, &frame),
            Err(CryptoError::UnexpectedFrame)
        );
    }

    #[test]
    fn empty_unknown_and_oversize_frames_are_refused() {
        assert_eq!(kind(&[]), Err(CryptoError::UnexpectedFrame));
        assert_eq!(kind(&[0, 1, 2]), Err(CryptoError::UnexpectedFrame));
        for byte in 0..=u8::MAX {
            if !FrameKind::ALL.iter().any(|kind| *kind as u8 == byte) {
                assert_eq!(kind(&[byte]), Err(CryptoError::UnexpectedFrame), "{byte}");
            }
        }
        let mut oversize = vec![FrameKind::Record as u8];
        oversize.resize(2 + MAX_RECORD_LEN, 0);
        assert_eq!(kind(&oversize), Err(CryptoError::UnexpectedFrame));
    }
}
