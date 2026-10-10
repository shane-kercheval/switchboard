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
use crate::noise::ECHO_LEN;
use crate::session::SESSION_ID_LEN;

/// Longest frame of any kind: the type byte, the longest prefix a frame
/// carries before its Noise message (a reply's echo), and the longest Noise
/// message. Anything longer is refused before anything is allocated for it.
pub const MAX_FRAME_LEN: usize = 1 + ECHO_LEN + MAX_RECORD_LEN;

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
    /// The phone's pairing messages, 1 and 3.
    Pairing = 1,
    /// A phone opening a connection: `Noise_KK` message 1.
    SessionRequest = 2,
    /// The Mac's answer: `Noise_KK` message 2.
    SessionReply = 3,
    /// An encrypted fragment on an established session.
    Record = 4,
    /// The Mac's pairing message 2. Its own type, like `SessionReply`, so a
    /// phone's message 1 sent back to it is refused by type rather than read
    /// as the Mac's answer.
    PairingReply = 5,
}

/// Classifies a frame without touching any handshake or session state. Fails
/// for an empty frame, an unknown type, or a frame longer than
/// `MAX_FRAME_LEN` — the last is refused here, before anything is allocated
/// for it.
pub fn kind(frame: &[u8]) -> Result<FrameKind, CryptoError> {
    if frame.len() > MAX_FRAME_LEN {
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

/// Longest frame of `kind`: the type byte, the prefix that kind carries before
/// its Noise message, and the longest Noise message.
const fn max_len(kind: FrameKind) -> usize {
    let prefix = match kind {
        FrameKind::Pairing | FrameKind::SessionRequest => 0,
        FrameKind::SessionReply | FrameKind::PairingReply => ECHO_LEN,
        FrameKind::Record => SESSION_ID_LEN,
    };
    1 + prefix + MAX_RECORD_LEN
}

/// The body of a frame of the expected kind. A frame longer than its kind
/// allows is refused here, so a step never hands one to `snow`.
pub(crate) fn body(expected: FrameKind, frame: &[u8]) -> Result<&[u8], CryptoError> {
    if kind(frame)? == expected && frame.len() <= max_len(expected) {
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
        oversize.resize(MAX_FRAME_LEN + 1, 0);
        assert_eq!(kind(&oversize), Err(CryptoError::UnexpectedFrame));
        oversize.truncate(MAX_FRAME_LEN);
        assert_eq!(kind(&oversize), Ok(FrameKind::Record));
    }

    #[test]
    fn each_kind_is_refused_one_byte_over_its_own_maximum() {
        for &kind in FrameKind::ALL {
            let mut frame = vec![kind as u8];
            frame.resize(max_len(kind), 0);
            assert!(body(kind, &frame).is_ok(), "{kind:?}");
            frame.push(0);
            assert_eq!(
                body(kind, &frame),
                Err(CryptoError::UnexpectedFrame),
                "{kind:?}"
            );
        }
    }

    #[test]
    fn the_overall_maximum_is_the_longest_kind() {
        let longest = FrameKind::ALL.iter().map(|&kind| max_len(kind)).max();
        assert_eq!(longest, Some(MAX_FRAME_LEN));
    }
}
