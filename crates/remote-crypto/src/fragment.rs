//! Splits a serialized envelope into fragments that each fit one Noise
//! transport message, and reassembles them.
//!
//! Every fragment starts with a header — message id, index, count — and the
//! session encrypts header and chunk together, so the header is authenticated.
//! The reassembler trusts nothing about it: a fragment that breaks a rule is an
//! error, and the caller tears the connection down.
//!
//! Records arrive in order (the transport rejects anything else) and one
//! `seal` produces all of an envelope's fragments together, so an honest peer
//! only ever sends a message's fragments contiguously and in order. The
//! reassembler accepts exactly that and nothing else.

use thiserror::Error;

/// Largest Noise transport message on the wire.
pub const MAX_RECORD_LEN: usize = 65_535;
/// The `ChaChaPoly` authentication tag every record carries.
pub const TAG_LEN: usize = 16;
/// Largest plaintext one record can carry.
pub const MAX_FRAGMENT_LEN: usize = MAX_RECORD_LEN - TAG_LEN;
/// Message id (u32), index (u16), count (u16), all big-endian.
const HEADER_LEN: usize = 8;
/// Largest slice of a message one fragment carries.
pub const MAX_CHUNK_LEN: usize = MAX_FRAGMENT_LEN - HEADER_LEN;
/// Largest message the reassembler accepts.
pub const MAX_MESSAGE_LEN: usize = 8 * 1024 * 1024;
/// Most fragments a message within `MAX_MESSAGE_LEN` can need.
const MAX_FRAGMENT_COUNT: usize = MAX_MESSAGE_LEN.div_ceil(MAX_CHUNK_LEN);

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum FragmentError {
    #[error("message of {len} bytes exceeds the {MAX_MESSAGE_LEN}-byte limit")]
    MessageTooLarge { len: usize },
    #[error("fragment of {len} bytes is shorter than its header")]
    Truncated { len: usize },
    #[error("fragment of {len} bytes exceeds the {MAX_FRAGMENT_LEN}-byte limit")]
    FragmentTooLarge { len: usize },
    #[error("fragment count {count} is outside 1..={MAX_FRAGMENT_COUNT}")]
    InvalidCount { count: u16 },
    #[error("fragment index {index} is outside a message of {count} fragments")]
    IndexOutOfRange { index: u16, count: u16 },
    #[error("fragment {index} of message {message_id} is out of sequence")]
    OutOfSequence { message_id: u32, index: u16 },
}

/// Numbers the messages it splits; one per sending direction of a session.
#[derive(Debug, Default)]
pub struct Fragmenter {
    next_message_id: u32,
}

/// The one way fragmenting fails: the message is over `MAX_MESSAGE_LEN`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("message of {len} bytes exceeds the {MAX_MESSAGE_LEN}-byte limit")]
pub struct TooLarge {
    pub len: usize,
}

impl Fragmenter {
    /// Splits `message` into fragments of at most `MAX_FRAGMENT_LEN` bytes. It
    /// never truncates: a message the reassembler would refuse is an error
    /// here. An empty message is one empty fragment.
    pub fn fragment(&mut self, message: &[u8]) -> Result<Vec<Vec<u8>>, TooLarge> {
        let too_large = TooLarge { len: message.len() };
        if message.len() > MAX_MESSAGE_LEN {
            return Err(too_large);
        }
        let message_id = self.next_message_id;
        self.next_message_id = self.next_message_id.wrapping_add(1);

        let chunks: Vec<&[u8]> = if message.is_empty() {
            vec![message]
        } else {
            message.chunks(MAX_CHUNK_LEN).collect()
        };
        // At most MAX_FRAGMENT_COUNT (129), so both conversions hold.
        let count = u16::try_from(chunks.len()).map_err(|_| too_large)?;
        let mut fragments = Vec::with_capacity(chunks.len());
        for (index, chunk) in chunks.into_iter().enumerate() {
            let index = u16::try_from(index).map_err(|_| too_large)?;
            let mut fragment = Vec::with_capacity(HEADER_LEN + chunk.len());
            fragment.extend_from_slice(&message_id.to_be_bytes());
            fragment.extend_from_slice(&index.to_be_bytes());
            fragment.extend_from_slice(&count.to_be_bytes());
            fragment.extend_from_slice(chunk);
            fragments.push(fragment);
        }
        Ok(fragments)
    }
}

/// Collects fragments into messages; one per receiving direction of a
/// session. At most one message is in progress, and messages arrive in the
/// order the peer's `Fragmenter` numbered them. Drop it, or call `reset`, when
/// the connection ends: a partial message never survives a disconnect.
#[derive(Debug, Default)]
pub struct Reassembler {
    in_progress: Option<Partial>,
    next_message_id: u32,
}

#[derive(Debug)]
struct Partial {
    message_id: u32,
    count: u16,
    next_index: u16,
    message: Vec<u8>,
}

struct Header {
    message_id: u32,
    index: u16,
    count: u16,
}

impl Reassembler {
    /// Takes one decrypted fragment. Returns the whole message once its last
    /// fragment arrives, `None` while it is still incomplete. A fragment must be
    /// the first of the next message — its id one past the last message's,
    /// wrapping — or the next fragment of the one in progress, with the same
    /// count. Any other fragment, and any malformed one, clears all state and is
    /// an error; the caller ends the session, because an authenticated peer sent
    /// something its `Fragmenter` never produces.
    pub fn accept(&mut self, fragment: &[u8]) -> Result<Option<Vec<u8>>, FragmentError> {
        let result = parse(fragment).and_then(|(header, chunk)| self.accept_chunk(&header, chunk));
        if result.is_err() {
            self.reset();
        }
        result
    }

    /// Discards all state, as at the start of a connection.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    #[cfg(test)]
    pub fn is_assembling(&self) -> bool {
        self.in_progress.is_some()
    }

    fn accept_chunk(
        &mut self,
        header: &Header,
        chunk: &[u8],
    ) -> Result<Option<Vec<u8>>, FragmentError> {
        let out_of_sequence = FragmentError::OutOfSequence {
            message_id: header.message_id,
            index: header.index,
        };
        let Some(partial) = self.in_progress.as_mut() else {
            if header.index != 0 || header.message_id != self.next_message_id {
                return Err(out_of_sequence);
            }
            self.next_message_id = header.message_id.wrapping_add(1);
            if header.count == 1 {
                return Ok(Some(chunk.to_vec()));
            }
            self.in_progress = Some(Partial {
                message_id: header.message_id,
                count: header.count,
                next_index: 1,
                message: chunk.to_vec(),
            });
            return Ok(None);
        };
        if header.message_id != partial.message_id
            || header.index != partial.next_index
            || header.count != partial.count
        {
            return Err(out_of_sequence);
        }
        let len = partial.message.len() + chunk.len();
        if len > MAX_MESSAGE_LEN {
            return Err(FragmentError::MessageTooLarge { len });
        }
        partial.message.extend_from_slice(chunk);
        partial.next_index += 1;
        if partial.next_index < partial.count {
            return Ok(None);
        }
        Ok(self.in_progress.take().map(|partial| partial.message))
    }
}

fn parse(fragment: &[u8]) -> Result<(Header, &[u8]), FragmentError> {
    if fragment.len() > MAX_FRAGMENT_LEN {
        return Err(FragmentError::FragmentTooLarge {
            len: fragment.len(),
        });
    }
    let Some((head, chunk)) = fragment.split_first_chunk::<HEADER_LEN>() else {
        return Err(FragmentError::Truncated {
            len: fragment.len(),
        });
    };
    let message_id = u32::from_be_bytes([head[0], head[1], head[2], head[3]]);
    let index = u16::from_be_bytes([head[4], head[5]]);
    let count = u16::from_be_bytes([head[6], head[7]]);
    if count == 0 || usize::from(count) > MAX_FRAGMENT_COUNT {
        return Err(FragmentError::InvalidCount { count });
    }
    if index >= count {
        return Err(FragmentError::IndexOutOfRange { index, count });
    }
    Ok((
        Header {
            message_id,
            index,
            count,
        },
        chunk,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(len: usize) -> Vec<u8> {
        (0..len).map(|i| u8::try_from(i % 251).unwrap()).collect()
    }

    fn reassemble(fragments: &[Vec<u8>]) -> Result<Option<Vec<u8>>, FragmentError> {
        let mut reassembler = Reassembler::default();
        let mut last = None;
        for fragment in fragments {
            last = reassembler.accept(fragment)?;
        }
        Ok(last)
    }

    fn header(message_id: u32, index: u16, count: u16) -> Vec<u8> {
        let mut fragment = message_id.to_be_bytes().to_vec();
        fragment.extend_from_slice(&index.to_be_bytes());
        fragment.extend_from_slice(&count.to_be_bytes());
        fragment
    }

    #[test]
    fn messages_round_trip_across_fragment_boundaries() {
        for len in [
            0,
            1,
            MAX_CHUNK_LEN - 1,
            MAX_CHUNK_LEN,
            MAX_CHUNK_LEN + 1,
            3 * MAX_CHUNK_LEN + 17,
            MAX_MESSAGE_LEN,
        ] {
            let original = message(len);
            let fragments = Fragmenter::default().fragment(&original).unwrap();
            assert!(
                fragments.iter().all(|f| f.len() <= MAX_FRAGMENT_LEN),
                "{len}"
            );
            assert_eq!(reassemble(&fragments), Ok(Some(original)), "{len}");
        }
    }

    #[test]
    fn a_message_above_the_limit_is_refused_rather_than_truncated() {
        assert_eq!(
            Fragmenter::default().fragment(&message(MAX_MESSAGE_LEN + 1)),
            Err(TooLarge {
                len: MAX_MESSAGE_LEN + 1
            })
        );
    }

    fn assert_out_of_sequence(
        reassembler: &mut Reassembler,
        fragment: &[u8],
        message_id: u32,
        index: u16,
    ) {
        assert_eq!(
            reassembler.accept(fragment),
            Err(FragmentError::OutOfSequence { message_id, index })
        );
        assert!(!reassembler.is_assembling());
    }

    #[test]
    fn a_message_missing_a_fragment_is_rejected_at_the_gap() {
        let fragments = Fragmenter::default()
            .fragment(&message(2 * MAX_CHUNK_LEN + 5))
            .unwrap();
        let mut reassembler = Reassembler::default();
        assert_eq!(reassembler.accept(&fragments[0]), Ok(None));
        assert_out_of_sequence(&mut reassembler, &fragments[2], 0, 2);
    }

    #[test]
    fn a_message_must_start_at_its_first_fragment() {
        let fragments = Fragmenter::default()
            .fragment(&message(2 * MAX_CHUNK_LEN))
            .unwrap();
        assert_out_of_sequence(&mut Reassembler::default(), &fragments[1], 0, 1);
    }

    #[test]
    fn a_repeated_fragment_is_rejected() {
        let fragments = Fragmenter::default()
            .fragment(&message(2 * MAX_CHUNK_LEN))
            .unwrap();
        let mut reassembler = Reassembler::default();
        assert_eq!(reassembler.accept(&fragments[0]), Ok(None));
        assert_out_of_sequence(&mut reassembler, &fragments[0], 0, 0);
    }

    #[test]
    fn a_new_message_before_the_current_one_completes_is_rejected() {
        let mut reassembler = Reassembler::default();
        assert_eq!(reassembler.accept(&header(0, 0, 2)), Ok(None));
        assert_out_of_sequence(&mut reassembler, &header(1, 0, 2), 1, 0);
        assert_eq!(reassembler.accept(&header(0, 0, 2)), Ok(None));
        assert_out_of_sequence(&mut reassembler, &header(1, 0, 1), 1, 0);
    }

    #[test]
    fn a_fragment_changing_its_message_count_is_rejected() {
        let mut reassembler = Reassembler::default();
        assert_eq!(reassembler.accept(&header(0, 0, 3)), Ok(None));
        assert_out_of_sequence(&mut reassembler, &header(0, 1, 2), 0, 1);
    }

    #[test]
    fn messages_must_arrive_in_the_order_they_were_numbered() {
        assert_out_of_sequence(&mut Reassembler::default(), &header(5, 0, 1), 5, 0);

        let mut reassembler = Reassembler::default();
        assert_eq!(reassembler.accept(&header(0, 0, 1)), Ok(Some(Vec::new())));
        assert_out_of_sequence(&mut reassembler, &header(0, 0, 1), 0, 0);

        let mut reassembler = Reassembler::default();
        assert_eq!(reassembler.accept(&header(0, 0, 1)), Ok(Some(Vec::new())));
        assert_out_of_sequence(&mut reassembler, &header(2, 0, 1), 2, 0);
    }

    #[test]
    fn message_ids_wrap_like_the_fragmenter_s() {
        let mut reassembler = Reassembler {
            next_message_id: u32::MAX,
            ..Reassembler::default()
        };
        assert_eq!(
            reassembler.accept(&header(u32::MAX, 0, 1)),
            Ok(Some(Vec::new()))
        );
        assert_eq!(reassembler.accept(&header(0, 0, 1)), Ok(Some(Vec::new())));
    }

    #[test]
    fn a_skipped_index_is_rejected() {
        let mut reassembler = Reassembler::default();
        assert_eq!(reassembler.accept(&header(0, 0, 3)), Ok(None));
        assert_out_of_sequence(&mut reassembler, &header(0, 2, 3), 0, 2);
    }

    #[test]
    fn malformed_input_after_a_message_has_started_clears_it() {
        let mut reassembler = Reassembler::default();
        assert_eq!(reassembler.accept(&header(0, 0, 2)), Ok(None));
        assert_eq!(
            reassembler.accept(&[0; HEADER_LEN - 1]),
            Err(FragmentError::Truncated {
                len: HEADER_LEN - 1
            })
        );
        assert!(!reassembler.is_assembling());
        assert_out_of_sequence(&mut reassembler, &header(0, 1, 2), 0, 1);
    }

    #[test]
    fn reset_discards_the_message_in_progress() {
        let mut reassembler = Reassembler::default();
        assert_eq!(reassembler.accept(&header(0, 0, 2)), Ok(None));
        reassembler.reset();
        assert!(!reassembler.is_assembling());
        assert_out_of_sequence(&mut reassembler, &header(0, 1, 2), 0, 1);
    }

    #[test]
    fn an_index_outside_the_count_is_rejected() {
        let mut fragment = header(0, 2, 2);
        fragment.push(1);
        assert_eq!(
            Reassembler::default().accept(&fragment),
            Err(FragmentError::IndexOutOfRange { index: 2, count: 2 })
        );
    }

    #[test]
    fn a_count_of_zero_or_beyond_the_size_limit_is_rejected() {
        for count in [0, u16::try_from(MAX_FRAGMENT_COUNT + 1).unwrap(), u16::MAX] {
            assert_eq!(
                Reassembler::default().accept(&header(0, 0, count)),
                Err(FragmentError::InvalidCount { count })
            );
        }
    }

    #[test]
    fn an_oversize_fragment_is_rejected() {
        let mut fragment = header(0, 0, 1);
        fragment.resize(MAX_FRAGMENT_LEN + 1, 0);
        assert_eq!(
            Reassembler::default().accept(&fragment),
            Err(FragmentError::FragmentTooLarge {
                len: MAX_FRAGMENT_LEN + 1
            })
        );
    }

    #[test]
    fn a_fragment_shorter_than_its_header_is_rejected() {
        assert_eq!(
            Reassembler::default().accept(&[0; HEADER_LEN - 1]),
            Err(FragmentError::Truncated {
                len: HEADER_LEN - 1
            })
        );
    }

    /// Full-size chunks within the count limit can still add up past 8 MiB,
    /// because the count limit rounds up.
    #[test]
    fn a_message_growing_past_the_size_limit_is_rejected() {
        let count = u16::try_from(MAX_FRAGMENT_COUNT).unwrap();
        let mut reassembler = Reassembler::default();
        let mut result = Ok(None);
        for index in 0..count {
            let mut fragment = header(0, index, count);
            fragment.resize(MAX_FRAGMENT_LEN, 0);
            result = reassembler.accept(&fragment);
            if result.is_err() {
                break;
            }
        }
        assert!(
            matches!(result, Err(FragmentError::MessageTooLarge { len }) if len > MAX_MESSAGE_LEN),
            "{result:?}"
        );
        assert!(!reassembler.is_assembling());
    }

    #[test]
    fn consecutive_messages_get_distinct_ids() {
        let mut fragmenter = Fragmenter::default();
        let first = fragmenter.fragment(b"a").unwrap();
        let second = fragmenter.fragment(b"b").unwrap();
        assert_ne!(first[0][..4], second[0][..4]);
    }
}
