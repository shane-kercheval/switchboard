use sha2::{Digest, Sha256};

/// Length of a `Noise_*_SHA256` handshake hash.
pub const HANDSHAKE_HASH_LEN: usize = 32;

const CODE_SPACE: u64 = 1_000_000;

/// Domain separation, so the code can never coincide with another value
/// derived from the same handshake hash.
const DOMAIN: &[u8] = b"switchboard pairing confirmation code v1";

#[derive(Debug, PartialEq, Eq, thiserror::Error, uniffi::Error)]
pub enum CryptoError {
    #[error("handshake hash must be {HANDSHAKE_HASH_LEN} bytes, got {length}")]
    InvalidHandshakeHash { length: u64 },
}

/// The six-digit code both screens show during pairing.
///
/// Both ends of one handshake share its hash, so they derive the same code; an
/// attacker who completed a different handshake derives a different one. The
/// derivation is part of the pairing protocol: a Mac and a phone built from
/// different versions must still agree, so changing it needs a new `DOMAIN`.
#[uniffi::export]
#[expect(
    clippy::needless_pass_by_value,
    reason = "UniFFI lifts byte sequences from Swift as an owned Vec"
)]
pub fn confirmation_code(handshake_hash: Vec<u8>) -> Result<String, CryptoError> {
    if handshake_hash.len() != HANDSHAKE_HASH_LEN {
        return Err(CryptoError::InvalidHandshakeHash {
            length: handshake_hash.len() as u64,
        });
    }
    let digest = Sha256::new()
        .chain_update(DOMAIN)
        .chain_update(&handshake_hash)
        .finalize();
    let mut prefix = [0u8; 8];
    prefix.copy_from_slice(&digest[..8]);
    // Reducing a 64-bit value modulo 10^6 biases the result by under 10^-13.
    let code = u64::from_be_bytes(prefix) % CODE_SPACE;
    Ok(format!("{code:06}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash(byte: u8) -> Vec<u8> {
        vec![byte; HANDSHAKE_HASH_LEN]
    }

    #[test]
    fn same_hash_yields_same_code() {
        assert_eq!(confirmation_code(hash(7)), confirmation_code(hash(7)));
    }

    #[test]
    fn different_hashes_yield_different_codes() {
        let mut other = hash(7);
        other[HANDSHAKE_HASH_LEN - 1] ^= 1;
        assert_ne!(
            confirmation_code(hash(7)).unwrap(),
            confirmation_code(other).unwrap()
        );
    }

    #[test]
    fn code_is_six_ascii_digits() {
        for byte in 0..=u8::MAX {
            let code = confirmation_code(hash(byte)).unwrap();
            assert_eq!(code.len(), 6, "{code}");
            assert!(code.bytes().all(|b| b.is_ascii_digit()), "{code}");
        }
    }

    /// Pins the derivation so a change to it fails here rather than as a
    /// pairing that never confirms between two builds. The expected value was
    /// computed independently of this crate, and its leading zero covers the
    /// padding.
    #[test]
    fn derivation_is_pinned() {
        assert_eq!(confirmation_code(hash(0)).unwrap(), "024132");
    }

    #[test]
    fn rejects_a_hash_of_the_wrong_length() {
        for length in [0, HANDSHAKE_HASH_LEN - 1, HANDSHAKE_HASH_LEN + 1] {
            assert_eq!(
                confirmation_code(vec![0; length]),
                Err(CryptoError::InvalidHandshakeHash {
                    length: length as u64
                })
            );
        }
    }
}
