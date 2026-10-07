//! Splits a serialized envelope into fragments that each fit one Noise
//! transport message, and reassembles them.
//!
//! Every fragment starts with a header — message id, index, count — and the
//! session encrypts header and chunk together, so the header is authenticated.
//! The reassembler trusts nothing about it: a fragment that breaks a rule is an
//! error, and the caller tears the connection down.

use std::collections::HashMap;

use thiserror::Error;

/// Largest Noise transport message on the wire.
pub const MAX_RECORD_LEN: usize = 65_535;
/// The `ChaChaPoly` authentication tag every record carries.
const TAG_LEN: usize = 16;
/// Largest plaintext one record can carry.
pub const MAX_FRAGMENT_LEN: usize = MAX_RECORD_LEN - TAG_LEN;
/// Message id (u32), index (u16), count (u16), all big-endian.
const HEADER_LEN: usize = 8;
/// Largest slice of a message one fragment carries.
pub const MAX_CHUNK_LEN: usize = MAX_FRAGMENT_LEN - HEADER_LEN;
/// Largest message the reassembler accepts.
pub const MAX_MESSAGE_LEN: usize = 8 * 1024 * 1024;
/// Most messages the reassembler holds partially at once.
pub const MAX_INCOMPLETE_MESSAGES: usize = 4;
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
    #[error("message {message_id} was announced with {expected} fragments, not {found}")]
    CountMismatch {
        message_id: u32,
        expected: u16,
        found: u16,
    },
    #[error("fragment {index} of message {message_id} arrived twice")]
    Duplicate { message_id: u32, index: u16 },
    #[error("more than {MAX_INCOMPLETE_MESSAGES} messages are incomplete")]
    TooManyIncomplete,
}

/// Numbers the messages it splits; one per sending direction of a session.
#[derive(Debug, Default)]
pub struct Fragmenter {
    next_message_id: u32,
}

impl Fragmenter {
    /// Splits `message` into fragments of at most `MAX_FRAGMENT_LEN` bytes. It
    /// never truncates: a message the reassembler would refuse is an error
    /// here. An empty message is one empty fragment.
    pub fn fragment(&mut self, message: &[u8]) -> Result<Vec<Vec<u8>>, FragmentError> {
        if message.len() > MAX_MESSAGE_LEN {
            return Err(FragmentError::MessageTooLarge { len: message.len() });
        }
        let message_id = self.next_message_id;
        self.next_message_id = self.next_message_id.wrapping_add(1);

        let chunks: Vec<&[u8]> = if message.is_empty() {
            vec![message]
        } else {
            message.chunks(MAX_CHUNK_LEN).collect()
        };
        // At most MAX_FRAGMENT_COUNT (129), so both conversions hold.
        let count = u16::try_from(chunks.len())
            .map_err(|_| FragmentError::MessageTooLarge { len: message.len() })?;
        let mut fragments = Vec::with_capacity(chunks.len());
        for (index, chunk) in chunks.into_iter().enumerate() {
            let index = u16::try_from(index)
                .map_err(|_| FragmentError::MessageTooLarge { len: message.len() })?;
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
/// session. Drop it, or call `reset`, when the connection ends: partial
/// messages never survive a disconnect.
#[derive(Debug, Default)]
pub struct Reassembler {
    incomplete: HashMap<u32, Partial>,
}

#[derive(Debug)]
struct Partial {
    count: u16,
    chunks: Vec<Option<Vec<u8>>>,
    received: usize,
    len: usize,
}

struct Header {
    message_id: u32,
    index: u16,
    count: u16,
}

impl Reassembler {
    /// Takes one decrypted fragment. Returns the whole message once its last
    /// fragment arrives, `None` while it is still incomplete. On an error the
    /// message the fragment belonged to is discarded; the caller should end
    /// the session, because an authenticated peer sent something malformed.
    pub fn accept(&mut self, fragment: &[u8]) -> Result<Option<Vec<u8>>, FragmentError> {
        let (header, chunk) = parse(fragment)?;
        let result = self.accept_chunk(&header, chunk);
        if result.is_err() {
            self.incomplete.remove(&header.message_id);
        }
        result
    }

    /// Discards every partial message.
    pub fn reset(&mut self) {
        self.incomplete.clear();
    }

    pub fn incomplete_messages(&self) -> usize {
        self.incomplete.len()
    }

    fn accept_chunk(
        &mut self,
        header: &Header,
        chunk: &[u8],
    ) -> Result<Option<Vec<u8>>, FragmentError> {
        if header.count == 1 && !self.incomplete.contains_key(&header.message_id) {
            return Ok(Some(chunk.to_vec()));
        }
        if !self.incomplete.contains_key(&header.message_id)
            && self.incomplete.len() >= MAX_INCOMPLETE_MESSAGES
        {
            return Err(FragmentError::TooManyIncomplete);
        }
        let partial = self
            .incomplete
            .entry(header.message_id)
            .or_insert_with(|| Partial {
                count: header.count,
                chunks: vec![None; usize::from(header.count)],
                received: 0,
                len: 0,
            });
        if partial.count != header.count {
            return Err(FragmentError::CountMismatch {
                message_id: header.message_id,
                expected: partial.count,
                found: header.count,
            });
        }
        let slot = &mut partial.chunks[usize::from(header.index)];
        if slot.is_some() {
            return Err(FragmentError::Duplicate {
                message_id: header.message_id,
                index: header.index,
            });
        }
        partial.len += chunk.len();
        if partial.len > MAX_MESSAGE_LEN {
            return Err(FragmentError::MessageTooLarge { len: partial.len });
        }
        *slot = Some(chunk.to_vec());
        partial.received += 1;
        if partial.received < usize::from(partial.count) {
            return Ok(None);
        }
        let Some(partial) = self.incomplete.remove(&header.message_id) else {
            return Ok(None);
        };
        let mut message = Vec::with_capacity(partial.len);
        for chunk in partial.chunks.into_iter().flatten() {
            message.extend_from_slice(&chunk);
        }
        Ok(Some(message))
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
            Err(FragmentError::MessageTooLarge {
                len: MAX_MESSAGE_LEN + 1
            })
        );
    }

    #[test]
    fn a_message_missing_a_fragment_never_completes() {
        let mut fragments = Fragmenter::default()
            .fragment(&message(2 * MAX_CHUNK_LEN + 5))
            .unwrap();
        fragments.remove(1);
        let mut reassembler = Reassembler::default();
        for fragment in &fragments {
            assert_eq!(reassembler.accept(fragment), Ok(None));
        }
        assert_eq!(reassembler.incomplete_messages(), 1);
    }

    #[test]
    fn fragments_arriving_out_of_order_still_assemble_in_order() {
        let original = message(3 * MAX_CHUNK_LEN);
        let mut fragments = Fragmenter::default().fragment(&original).unwrap();
        fragments.reverse();
        assert_eq!(reassemble(&fragments), Ok(Some(original)));
    }

    #[test]
    fn a_duplicate_fragment_is_rejected_and_discards_its_message() {
        let fragments = Fragmenter::default()
            .fragment(&message(2 * MAX_CHUNK_LEN))
            .unwrap();
        let mut reassembler = Reassembler::default();
        assert_eq!(reassembler.accept(&fragments[0]), Ok(None));
        assert_eq!(
            reassembler.accept(&fragments[0]),
            Err(FragmentError::Duplicate {
                message_id: 0,
                index: 0
            })
        );
        assert_eq!(reassembler.incomplete_messages(), 0);
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
    fn a_fragment_disagreeing_on_its_message_count_is_rejected() {
        let mut reassembler = Reassembler::default();
        assert_eq!(reassembler.accept(&header(7, 0, 3)), Ok(None));
        assert_eq!(
            reassembler.accept(&header(7, 1, 2)),
            Err(FragmentError::CountMismatch {
                message_id: 7,
                expected: 3,
                found: 2
            })
        );
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
        assert_eq!(reassembler.incomplete_messages(), 0);
    }

    #[test]
    fn a_fifth_incomplete_message_is_rejected() {
        let mut reassembler = Reassembler::default();
        for message_id in 0..4 {
            assert_eq!(reassembler.accept(&header(message_id, 0, 2)), Ok(None));
        }
        assert_eq!(
            reassembler.accept(&header(4, 0, 2)),
            Err(FragmentError::TooManyIncomplete)
        );
        assert_eq!(reassembler.incomplete_messages(), 4);
        assert_eq!(reassembler.accept(&header(4, 0, 1)), Ok(Some(Vec::new())));
    }

    #[test]
    fn reset_discards_partial_messages() {
        let mut reassembler = Reassembler::default();
        assert_eq!(reassembler.accept(&header(0, 0, 2)), Ok(None));
        reassembler.reset();
        assert_eq!(reassembler.incomplete_messages(), 0);
        assert_eq!(reassembler.accept(&header(0, 1, 2)), Ok(None));
    }

    #[test]
    fn consecutive_messages_get_distinct_ids() {
        let mut fragmenter = Fragmenter::default();
        let first = fragmenter.fragment(b"a").unwrap();
        let second = fragmenter.fragment(b"b").unwrap();
        assert_ne!(first[0][..4], second[0][..4]);
    }
}
