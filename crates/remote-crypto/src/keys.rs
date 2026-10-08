//! A device's long-term keys: the X25519 static key its Noise handshakes use
//! and the Ed25519 identity key it registers with the relay.

use ed25519_dalek::{SigningKey, VerifyingKey};
use snow::params::DHChoice;
use snow::resolvers::{CryptoResolver, DefaultResolver};
use zeroize::{Zeroize, Zeroizing};

use crate::CryptoError;
use crate::identity::{self, Signature, SignaturePurpose};

pub const KEY_LEN: usize = 32;
const BLOB_VERSION: u8 = 1;
/// Version byte, Noise private key, Ed25519 seed.
const BLOB_LEN: usize = 1 + 2 * KEY_LEN;

/// Owns a device's private keys and never hands them out: handshakes take the
/// whole object, and the only way the private bytes leave is `storage_bytes`,
/// for the platform's secret store. The keys are wiped when it is dropped.
pub struct DeviceKeys {
    noise_private: Zeroizing<[u8; KEY_LEN]>,
    noise_public: [u8; KEY_LEN],
    identity: SigningKey,
}

impl DeviceKeys {
    /// Fresh keys from the operating system's random number generator.
    pub fn generate() -> Result<Self, CryptoError> {
        let mut noise_private = Zeroizing::new([0u8; KEY_LEN]);
        let mut identity_seed = Zeroizing::new([0u8; KEY_LEN]);
        getrandom::fill(noise_private.as_mut_slice())
            .and_then(|()| getrandom::fill(identity_seed.as_mut_slice()))
            .map_err(|_| CryptoError::RandomnessUnavailable)?;
        Self::from_secrets(&noise_private, &identity_seed)
    }

    /// Keys from `storage_bytes`. Rejects an unknown version or a wrong length.
    pub fn restore(blob: &[u8]) -> Result<Self, CryptoError> {
        if blob.len() != BLOB_LEN || blob[0] != BLOB_VERSION {
            return Err(CryptoError::InvalidKeyBlob);
        }
        let mut noise_private = Zeroizing::new([0u8; KEY_LEN]);
        let mut identity_seed = Zeroizing::new([0u8; KEY_LEN]);
        noise_private.copy_from_slice(&blob[1..=KEY_LEN]);
        identity_seed.copy_from_slice(&blob[1 + KEY_LEN..]);
        Self::from_secrets(&noise_private, &identity_seed)
    }

    /// A versioned blob for the platform's secret store, which must treat it
    /// as opaque. Wiped when the returned value is dropped.
    pub fn storage_bytes(&self) -> Zeroizing<Vec<u8>> {
        let mut blob = Zeroizing::new(Vec::with_capacity(BLOB_LEN));
        blob.push(BLOB_VERSION);
        blob.extend_from_slice(self.noise_private.as_slice());
        let mut identity_seed = self.identity.to_bytes();
        blob.extend_from_slice(&identity_seed);
        identity_seed.zeroize();
        blob
    }

    pub fn noise_public_key(&self) -> [u8; KEY_LEN] {
        self.noise_public
    }

    pub fn identity_public_key(&self) -> VerifyingKey {
        self.identity.verifying_key()
    }

    pub fn device_id(&self) -> String {
        identity::device_id(&self.identity.verifying_key())
    }

    /// Proves to the relay that this device holds the identity key behind its
    /// device id. The only public producer of a relay-challenge signature;
    /// `identity::verify_relay_challenge` is the matching check.
    pub fn sign_relay_challenge(&self, challenge: &[u8]) -> Signature {
        identity::sign(&self.identity, SignaturePurpose::RelayChallenge, challenge)
    }

    pub(crate) fn noise_private_key(&self) -> &[u8; KEY_LEN] {
        &self.noise_private
    }

    pub(crate) fn identity_signing_key(&self) -> &SigningKey {
        &self.identity
    }

    pub(crate) fn from_secrets(
        noise_private: &[u8; KEY_LEN],
        identity_seed: &[u8; KEY_LEN],
    ) -> Result<Self, CryptoError> {
        Ok(Self {
            noise_private: Zeroizing::new(*noise_private),
            noise_public: noise_public_key(noise_private)?,
            identity: SigningKey::from_bytes(identity_seed),
        })
    }
}

/// Derived by the same resolver the handshakes use, so the public key a peer
/// pins is exactly the one this device proves possession of.
fn noise_public_key(private: &[u8; KEY_LEN]) -> Result<[u8; KEY_LEN], CryptoError> {
    let mut dh = DefaultResolver
        .resolve_dh(&DHChoice::Curve25519)
        .ok_or(CryptoError::InvalidKey)?;
    dh.set(private);
    <[u8; KEY_LEN]>::try_from(dh.pubkey()).map_err(|_| CryptoError::InvalidKey)
}

impl std::fmt::Debug for DeviceKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceKeys")
            .field("device_id", &self.device_id())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(text: &str) -> [u8; KEY_LEN] {
        let mut out = [0u8; KEY_LEN];
        for (i, byte) in out.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&text[2 * i..2 * i + 2], 16).unwrap();
        }
        out
    }

    /// RFC 7748 §6.1: Alice's private key and the public key it must yield.
    #[test]
    fn the_noise_public_key_matches_rfc_7748() {
        let keys = DeviceKeys::from_secrets(
            &hex("77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a"),
            &[0; KEY_LEN],
        )
        .unwrap();
        assert_eq!(
            keys.noise_public_key(),
            hex("8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a")
        );
    }

    #[test]
    fn keys_round_trip_through_storage() {
        let keys = DeviceKeys::generate().unwrap();
        let restored = DeviceKeys::restore(&keys.storage_bytes()).unwrap();
        assert_eq!(restored.noise_public_key(), keys.noise_public_key());
        assert_eq!(restored.identity_public_key(), keys.identity_public_key());
        assert_eq!(restored.device_id(), keys.device_id());
    }

    #[test]
    fn generated_keys_differ() {
        let a = DeviceKeys::generate().unwrap();
        let b = DeviceKeys::generate().unwrap();
        assert_ne!(a.noise_public_key(), b.noise_public_key());
        assert_ne!(a.device_id(), b.device_id());
    }

    #[test]
    fn restore_rejects_an_unknown_version_and_a_wrong_length() {
        let blob = DeviceKeys::generate().unwrap().storage_bytes();
        let mut wrong_version = blob.to_vec();
        wrong_version[0] = 2;
        assert_eq!(
            DeviceKeys::restore(&wrong_version).unwrap_err(),
            CryptoError::InvalidKeyBlob
        );
        assert_eq!(
            DeviceKeys::restore(&blob[..BLOB_LEN - 1]).unwrap_err(),
            CryptoError::InvalidKeyBlob
        );
        let mut long = blob.to_vec();
        long.push(0);
        assert_eq!(
            DeviceKeys::restore(&long).unwrap_err(),
            CryptoError::InvalidKeyBlob
        );
        assert_eq!(
            DeviceKeys::restore(&[]).unwrap_err(),
            CryptoError::InvalidKeyBlob
        );
    }

    #[test]
    fn a_relay_challenge_signature_verifies_only_as_one() {
        let keys = DeviceKeys::generate().unwrap();
        let signature = keys.sign_relay_challenge(b"challenge");
        let public = keys.identity_public_key();
        assert_eq!(
            identity::verify_relay_challenge(&public, b"challenge", &signature),
            Ok(())
        );
        assert_eq!(
            identity::verify_relay_challenge(&public, b"other", &signature),
            Err(CryptoError::InvalidSignature)
        );
        assert_eq!(
            identity::verify(
                &public,
                SignaturePurpose::PairingBinding,
                b"challenge",
                &signature
            ),
            Err(CryptoError::InvalidSignature)
        );
    }

    #[test]
    fn debug_output_shows_no_key_material() {
        let keys = DeviceKeys::generate().unwrap();
        let text = format!("{keys:?}");
        assert!(text.contains(&keys.device_id()), "{text}");
        assert!(
            !text.contains("noise_private") && !text.contains("identity:"),
            "{text}"
        );
    }
}
