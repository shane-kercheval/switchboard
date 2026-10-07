//! Device identity: the Ed25519 key a device registers with the relay, the
//! device id derived from it, and domain-separated signatures.

use data_encoding::BASE32_NOPAD;
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use sha2::{Digest, Sha256};

use crate::CryptoError;

/// Length of a device id: 26 base32 characters, 130 bits of the key's hash.
pub const DEVICE_ID_LEN: usize = 26;

/// What a signature is for. Each purpose signs a different message prefix, so
/// a signature made for one purpose never verifies for another: a relay
/// cannot turn a registration signature into a pairing binding, or back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignaturePurpose {
    RelayChallenge,
    PairingBinding,
}

impl SignaturePurpose {
    fn context(self) -> &'static [u8] {
        match self {
            Self::RelayChallenge => b"switchboard relay challenge v1",
            Self::PairingBinding => b"switchboard pairing binding v1",
        }
    }

    fn message(self, payload: &[u8]) -> Vec<u8> {
        let context = self.context();
        let mut message = Vec::with_capacity(1 + context.len() + payload.len());
        // Contexts are short constants; the length prefix keeps one context
        // from being a prefix of another.
        message.push(u8::try_from(context.len()).unwrap_or(u8::MAX));
        message.extend_from_slice(context);
        message.extend_from_slice(payload);
        message
    }
}

/// The relay's and the Mac's name for a device: lower-case RFC 4648 base32 of
/// the SHA-256 of its identity public key, cut to 26 characters. Compare ids
/// as exact strings; never case-fold them.
pub fn device_id(identity_public_key: &VerifyingKey) -> String {
    let digest = Sha256::digest(identity_public_key.as_bytes());
    let mut id = BASE32_NOPAD.encode(&digest);
    id.truncate(DEVICE_ID_LEN);
    id.make_ascii_lowercase();
    id
}

pub fn sign(
    key: &SigningKey,
    purpose: SignaturePurpose,
    payload: &[u8],
) -> Result<Signature, CryptoError> {
    key.try_sign(&purpose.message(payload))
        .map_err(|_| CryptoError::InvalidSignature)
}

/// Strict verification: rejects non-canonical signatures and small-order
/// keys, so one statement has exactly one valid signature encoding.
pub fn verify(
    key: &VerifyingKey,
    purpose: SignaturePurpose,
    payload: &[u8],
    signature: &Signature,
) -> Result<(), CryptoError> {
    key.verify_strict(&purpose.message(payload), signature)
        .map_err(|_| CryptoError::InvalidSignature)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::hex;

    fn key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    #[test]
    fn device_id_is_26_lower_case_base32_characters() {
        for seed in 0..=u8::MAX {
            let id = device_id(&key(seed).verifying_key());
            assert_eq!(id.len(), DEVICE_ID_LEN, "{id}");
            assert!(
                id.bytes()
                    .all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b)),
                "{id}"
            );
        }
    }

    #[test]
    fn device_id_differs_per_key_and_is_stable() {
        let a = key(1).verifying_key();
        assert_eq!(device_id(&a), device_id(&a));
        assert_ne!(device_id(&a), device_id(&key(2).verifying_key()));
    }

    /// The id format is shared with the relay and stored in the Mac's device
    /// registry, so it is pinned. The key is RFC 8032's first test vector, and
    /// the expected id was computed independently from its published public key.
    #[test]
    fn device_id_derivation_is_pinned() {
        let secret = [
            0x9d, 0x61, 0xb1, 0x9d, 0xef, 0xfd, 0x5a, 0x60, 0xba, 0x84, 0x4a, 0xf4, 0x92, 0xec,
            0x2c, 0xc4, 0x44, 0x49, 0xc5, 0x69, 0x7b, 0x32, 0x69, 0x19, 0x70, 0x3b, 0xac, 0x03,
            0x1c, 0xae, 0x7f, 0x60,
        ];
        let public = SigningKey::from_bytes(&secret).verifying_key();
        assert_eq!(
            hex(public.as_bytes()),
            "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a"
        );
        assert_eq!(device_id(&public), "eh7ddx5bksrgcytl7bkai36se4");
    }

    #[test]
    fn a_signature_verifies_for_its_key_payload_and_purpose() {
        let signing = key(3);
        let signature = sign(&signing, SignaturePurpose::RelayChallenge, b"nonce").unwrap();
        assert_eq!(
            verify(
                &signing.verifying_key(),
                SignaturePurpose::RelayChallenge,
                b"nonce",
                &signature
            ),
            Ok(())
        );
    }

    #[test]
    fn verification_fails_for_a_wrong_key() {
        let signature = sign(&key(3), SignaturePurpose::RelayChallenge, b"nonce").unwrap();
        assert_eq!(
            verify(
                &key(4).verifying_key(),
                SignaturePurpose::RelayChallenge,
                b"nonce",
                &signature
            ),
            Err(CryptoError::InvalidSignature)
        );
    }

    #[test]
    fn verification_fails_for_a_changed_payload() {
        let signing = key(3);
        let signature = sign(&signing, SignaturePurpose::RelayChallenge, b"nonce").unwrap();
        assert_eq!(
            verify(
                &signing.verifying_key(),
                SignaturePurpose::RelayChallenge,
                b"nonc3",
                &signature
            ),
            Err(CryptoError::InvalidSignature)
        );
    }

    #[test]
    fn a_signature_for_one_purpose_does_not_verify_for_another() {
        let signing = key(3);
        let signature = sign(&signing, SignaturePurpose::RelayChallenge, b"payload").unwrap();
        assert_eq!(
            verify(
                &signing.verifying_key(),
                SignaturePurpose::PairingBinding,
                b"payload",
                &signature
            ),
            Err(CryptoError::InvalidSignature)
        );
    }
}
