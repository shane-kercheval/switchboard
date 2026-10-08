//! The one-time pairing handshake, `Noise_XXpsk2`, keyed by the PSK in the
//! Mac's QR code. The phone initiates; three messages:
//!
//! 1. phone → Mac: empty payload (nothing is encrypted yet).
//! 2. Mac → phone: version, the Mac's name.
//! 3. phone → Mac: version, the phone's Ed25519 identity key, a signature
//!    binding that identity key to the phone's Noise key and this handshake,
//!    the phone's name.
//!
//! Both sides then derive the confirmation code from the final handshake hash.

use ed25519_dalek::{Signature, SigningKey, VerifyingKey};
use snow::{Builder, HandshakeState};
use zeroize::Zeroizing;

use crate::frame::{self, FrameKind};
use crate::identity::{self, SignaturePurpose};
use crate::keys::{DeviceKeys, KEY_LEN};
use crate::{CryptoError, HANDSHAKE_HASH_LEN};

const PATTERN: &str = "Noise_XXpsk2_25519_ChaChaPoly_SHA256";
const PROLOGUE: &[u8] = b"switchboard pairing v1";
const PSK_LOCATION: u8 = 2;
pub const PSK_LEN: usize = 32;
const PAYLOAD_VERSION: u8 = 1;
/// Longest name either side sends, in bytes of UTF-8.
pub const MAX_NAME_LEN: usize = 64;
const SIGNATURE_LEN: usize = 64;
const MAX_MESSAGE_LEN: usize = 65_535;

/// What the phone reads from the Mac's QR code.
pub struct PairingOffer {
    pub mac_noise_public_key: [u8; KEY_LEN],
    pub psk: Zeroizing<[u8; PSK_LEN]>,
}

/// The phone's result: the Mac it would pin, and the hash its confirmation
/// code comes from. Not trusted yet: the phone stores these keys only when the
/// Mac reports that the user typed the matching code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnconfirmedMac {
    pub noise_public_key: [u8; KEY_LEN],
    pub name: String,
    pub handshake_hash: [u8; HANDSHAKE_HASH_LEN],
}

/// The Mac's result: a phone asking to be trusted. Only the pairing
/// coordinator turns a candidate into a trusted device, and only after the
/// user has typed the confirmation code the phone shows and it matches the
/// code derived here — the check that defeats someone who photographed the QR
/// code and pairs first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingCandidate {
    pub noise_public_key: [u8; KEY_LEN],
    pub identity_public_key: VerifyingKey,
    pub name: String,
    pub handshake_hash: [u8; HANDSHAKE_HASH_LEN],
}

/// The phone's state after sending message 1. A frame of another type leaves
/// it waiting; reading a pairing message, whatever the outcome, uses it up.
pub struct PhoneAwaitingResponse {
    handshake: Option<HandshakeState>,
    expected_mac_key: [u8; KEY_LEN],
    identity: SigningKey,
    noise_public_key: [u8; KEY_LEN],
}

/// The Mac's state after sending message 2. Like `PhoneAwaitingResponse`, a
/// frame of another type leaves it waiting.
pub struct MacAwaitingFinish {
    handshake: Option<HandshakeState>,
    /// The hash as it stood before message 3, which the phone's binding
    /// signature covers. Reading message 3 changes it.
    hash_before_message_3: [u8; HANDSHAKE_HASH_LEN],
}

/// Phone: start pairing with the Mac described by the QR code.
pub fn phone_start(
    keys: &DeviceKeys,
    offer: &PairingOffer,
) -> Result<(PhoneAwaitingResponse, Vec<u8>), CryptoError> {
    phone_start_with(keys, offer, None)
}

/// Mac: answer a phone's first message with this Mac's name.
pub fn mac_respond(
    keys: &DeviceKeys,
    psk: &[u8; PSK_LEN],
    mac_name: &str,
    message_1: &[u8],
) -> Result<(MacAwaitingFinish, Vec<u8>), CryptoError> {
    mac_respond_with(keys, psk, mac_name, message_1, None)
}

impl PhoneAwaitingResponse {
    /// Reads the Mac's message 2 and writes message 3, which ends the
    /// handshake on this side.
    pub fn respond(
        &mut self,
        phone_name: &str,
        message_2: &[u8],
    ) -> Result<(UnconfirmedMac, Vec<u8>), CryptoError> {
        self.respond_with(message_2, |identity, hash, noise_key| {
            message_3_payload(identity, hash, noise_key, phone_name)
        })
    }

    fn respond_with(
        &mut self,
        message_2: &[u8],
        build_payload: impl FnOnce(&SigningKey, &[u8; HANDSHAKE_HASH_LEN], &[u8; KEY_LEN]) -> Vec<u8>,
    ) -> Result<(UnconfirmedMac, Vec<u8>), CryptoError> {
        let message_2 = frame::body(FrameKind::Pairing, message_2)?;
        let mut handshake = self.handshake.take().ok_or(CryptoError::HandshakeFailed)?;
        let payload = read(&mut handshake, message_2)?;
        let mac_key = remote_static(&handshake)?;
        if mac_key != self.expected_mac_key {
            return Err(CryptoError::UnexpectedPeerKey);
        }
        let mac_name = decode_message_2(&payload)?;
        let hash = handshake_hash(&handshake)?;
        let payload = build_payload(&self.identity, &hash, &self.noise_public_key);
        let message_3 = write(&mut handshake, &payload)?;
        Ok((
            UnconfirmedMac {
                noise_public_key: mac_key,
                name: mac_name,
                handshake_hash: handshake_hash(&handshake)?,
            },
            frame::tagged(FrameKind::Pairing, &message_3),
        ))
    }
}

impl MacAwaitingFinish {
    /// Reads the phone's message 3: its keys, its name, and the signature
    /// proving it holds the identity key it registers.
    pub fn finish(&mut self, message_3: &[u8]) -> Result<PairingCandidate, CryptoError> {
        let message_3 = frame::body(FrameKind::Pairing, message_3)?;
        let mut handshake = self.handshake.take().ok_or(CryptoError::HandshakeFailed)?;
        let payload = read(&mut handshake, message_3)?;
        let phone_noise_key = remote_static(&handshake)?;
        let (identity_public_key, signature, name) = decode_message_3(&payload)?;
        identity::verify(
            &identity_public_key,
            SignaturePurpose::PairingBinding,
            &binding_statement(&self.hash_before_message_3, &phone_noise_key),
            &signature,
        )?;
        Ok(PairingCandidate {
            noise_public_key: phone_noise_key,
            identity_public_key,
            name,
            handshake_hash: handshake_hash(&handshake)?,
        })
    }
}

fn phone_start_with(
    keys: &DeviceKeys,
    offer: &PairingOffer,
    ephemeral: Option<&[u8; KEY_LEN]>,
) -> Result<(PhoneAwaitingResponse, Vec<u8>), CryptoError> {
    let mut handshake = builder(keys, &offer.psk, ephemeral)?
        .build_initiator()
        .map_err(|_| CryptoError::HandshakeFailed)?;
    let message_1 = write(&mut handshake, &[])?;
    Ok((
        PhoneAwaitingResponse {
            handshake: Some(handshake),
            expected_mac_key: offer.mac_noise_public_key,
            identity: keys.identity_signing_key().clone(),
            noise_public_key: keys.noise_public_key(),
        },
        frame::tagged(FrameKind::Pairing, &message_1),
    ))
}

fn mac_respond_with(
    keys: &DeviceKeys,
    psk: &[u8; PSK_LEN],
    mac_name: &str,
    message_1: &[u8],
    ephemeral: Option<&[u8; KEY_LEN]>,
) -> Result<(MacAwaitingFinish, Vec<u8>), CryptoError> {
    let mut handshake = builder(keys, psk, ephemeral)?
        .build_responder()
        .map_err(|_| CryptoError::HandshakeFailed)?;
    let message_1 = frame::body(FrameKind::Pairing, message_1)?;
    if !read(&mut handshake, message_1)?.is_empty() {
        return Err(CryptoError::InvalidPayload);
    }
    let mut payload = vec![PAYLOAD_VERSION];
    encode_name(mac_name, &mut payload);
    let message_2 = write(&mut handshake, &payload)?;
    let hash_before_message_3 = handshake_hash(&handshake)?;
    Ok((
        MacAwaitingFinish {
            handshake: Some(handshake),
            hash_before_message_3,
        },
        frame::tagged(FrameKind::Pairing, &message_2),
    ))
}

fn builder<'a>(
    keys: &'a DeviceKeys,
    psk: &'a [u8; PSK_LEN],
    ephemeral: Option<&'a [u8; KEY_LEN]>,
) -> Result<Builder<'a>, CryptoError> {
    let params = PATTERN.parse().map_err(|_| CryptoError::HandshakeFailed)?;
    let mut builder = Builder::new(params)
        .local_private_key(keys.noise_private_key())
        .and_then(|b| b.prologue(PROLOGUE))
        .and_then(|b| b.psk(PSK_LOCATION, psk))
        .map_err(|_| CryptoError::HandshakeFailed)?;
    if let Some(ephemeral) = ephemeral {
        builder = builder.fixed_ephemeral_key_for_testing_only(ephemeral);
    }
    Ok(builder)
}

pub(crate) fn read(handshake: &mut HandshakeState, message: &[u8]) -> Result<Vec<u8>, CryptoError> {
    // A handshake payload is never longer than its message, and a message
    // longer than the Noise maximum is refused before decryption.
    let mut payload = vec![0u8; message.len().min(MAX_MESSAGE_LEN)];
    let len = handshake
        .read_message(message, &mut payload)
        .map_err(|_| CryptoError::HandshakeFailed)?;
    payload.truncate(len);
    payload.shrink_to_fit();
    Ok(payload)
}

pub(crate) fn write(
    handshake: &mut HandshakeState,
    payload: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    let mut message = vec![0u8; MAX_MESSAGE_LEN];
    let len = handshake
        .write_message(payload, &mut message)
        .map_err(|_| CryptoError::HandshakeFailed)?;
    message.truncate(len);
    message.shrink_to_fit();
    Ok(message)
}

fn remote_static(handshake: &HandshakeState) -> Result<[u8; KEY_LEN], CryptoError> {
    handshake
        .get_remote_static()
        .and_then(|key| <[u8; KEY_LEN]>::try_from(key).ok())
        .ok_or(CryptoError::HandshakeFailed)
}

pub(crate) fn handshake_hash(
    handshake: &HandshakeState,
) -> Result<[u8; HANDSHAKE_HASH_LEN], CryptoError> {
    <[u8; HANDSHAKE_HASH_LEN]>::try_from(handshake.get_handshake_hash())
        .map_err(|_| CryptoError::HandshakeFailed)
}

/// What the phone's identity key signs: this handshake, as far as it has
/// gone, and the Noise key that identity vouches for.
fn binding_statement(hash: &[u8; HANDSHAKE_HASH_LEN], noise_public_key: &[u8; KEY_LEN]) -> Vec<u8> {
    let mut statement = Vec::with_capacity(HANDSHAKE_HASH_LEN + KEY_LEN);
    statement.extend_from_slice(hash);
    statement.extend_from_slice(noise_public_key);
    statement
}

fn message_3_payload(
    identity: &SigningKey,
    hash: &[u8; HANDSHAKE_HASH_LEN],
    noise_public_key: &[u8; KEY_LEN],
    phone_name: &str,
) -> Vec<u8> {
    let signature = identity::sign(
        identity,
        SignaturePurpose::PairingBinding,
        &binding_statement(hash, noise_public_key),
    );
    let mut payload = vec![PAYLOAD_VERSION];
    payload.extend_from_slice(identity.verifying_key().as_bytes());
    payload.extend_from_slice(&signature.to_bytes());
    encode_name(phone_name, &mut payload);
    payload
}

/// Appends a one-byte length and the name, cut at a character boundary to at
/// most `MAX_NAME_LEN` bytes.
fn encode_name(name: &str, out: &mut Vec<u8>) {
    let mut end = name.len().min(MAX_NAME_LEN);
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    let bytes = &name.as_bytes()[..end];
    out.push(u8::try_from(bytes.len()).unwrap_or(0));
    out.extend_from_slice(bytes);
}

/// Reads a name written by `encode_name` and returns it without characters
/// that can disguise text next to the confirmation code: control characters,
/// the Unicode `Bidi_Control` set (which reorders text), and the zero-width
/// spaces and invisible operators (which hide it). Zero-width joiners stay,
/// because scripts and emoji sequences need them. Apps still render names as
/// isolated, single-line text, which covers anything this misses.
fn decode_name(input: &[u8]) -> Result<(String, &[u8]), CryptoError> {
    let (&len, rest) = input.split_first().ok_or(CryptoError::InvalidPayload)?;
    let len = usize::from(len);
    if len > MAX_NAME_LEN || rest.len() < len {
        return Err(CryptoError::InvalidPayload);
    }
    let (bytes, rest) = rest.split_at(len);
    let name = std::str::from_utf8(bytes).map_err(|_| CryptoError::InvalidPayload)?;
    Ok((name.chars().filter(|&c| is_displayable(c)).collect(), rest))
}

fn is_displayable(c: char) -> bool {
    !c.is_control()
        && !matches!(
            c,
            // Bidi_Control.
            '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
            // Line and paragraph separators (`Zl`, `Zp`), which text views
            // honour as line breaks but `is_control` does not cover.
            | '\u{2028}' | '\u{2029}'
            // Zero-width space, word joiner and invisible operators, and the
            // byte order mark.
            | '\u{200B}' | '\u{2060}'..='\u{2064}' | '\u{FEFF}'
        )
}

fn decode_message_2(payload: &[u8]) -> Result<String, CryptoError> {
    let rest = expect_version(payload)?;
    let (name, rest) = decode_name(rest)?;
    if !rest.is_empty() {
        return Err(CryptoError::InvalidPayload);
    }
    Ok(name)
}

fn decode_message_3(payload: &[u8]) -> Result<(VerifyingKey, Signature, String), CryptoError> {
    let rest = expect_version(payload)?;
    let (key, rest) = rest
        .split_first_chunk::<KEY_LEN>()
        .ok_or(CryptoError::InvalidPayload)?;
    let (signature, rest) = rest
        .split_first_chunk::<SIGNATURE_LEN>()
        .ok_or(CryptoError::InvalidPayload)?;
    let (name, rest) = decode_name(rest)?;
    if !rest.is_empty() {
        return Err(CryptoError::InvalidPayload);
    }
    let key = VerifyingKey::from_bytes(key).map_err(|_| CryptoError::InvalidPayload)?;
    Ok((key, Signature::from_bytes(signature), name))
}

fn expect_version(payload: &[u8]) -> Result<&[u8], CryptoError> {
    match payload.split_first() {
        Some((&PAYLOAD_VERSION, rest)) => Ok(rest),
        _ => Err(CryptoError::InvalidPayload),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::confirmation_code;
    use crate::test_support::hex;

    struct Party {
        phone: DeviceKeys,
        mac: DeviceKeys,
        psk: [u8; PSK_LEN],
    }

    fn party() -> Party {
        Party {
            phone: DeviceKeys::from_secrets(&[1; KEY_LEN], &[2; KEY_LEN]).unwrap(),
            mac: DeviceKeys::from_secrets(&[3; KEY_LEN], &[4; KEY_LEN]).unwrap(),
            psk: [5; PSK_LEN],
        }
    }

    fn offer(party: &Party) -> PairingOffer {
        PairingOffer {
            mac_noise_public_key: party.mac.noise_public_key(),
            psk: Zeroizing::new(party.psk),
        }
    }

    fn pair(party: &Party) -> (UnconfirmedMac, PairingCandidate) {
        let (mut phone, message_1) = phone_start(&party.phone, &offer(party)).unwrap();
        let (mut mac, message_2) =
            mac_respond(&party.mac, &party.psk, "Studio Mac", &message_1).unwrap();
        let (paired_mac, message_3) = phone.respond("Jo's iPhone", &message_2).unwrap();
        (paired_mac, mac.finish(&message_3).unwrap())
    }

    #[test]
    fn both_sides_learn_each_other_and_agree_on_the_code() {
        let party = party();
        let (paired_mac, paired_phone) = pair(&party);
        assert_eq!(paired_mac.noise_public_key, party.mac.noise_public_key());
        assert_eq!(paired_mac.name, "Studio Mac");
        assert_eq!(
            paired_phone.noise_public_key,
            party.phone.noise_public_key()
        );
        assert_eq!(
            paired_phone.identity_public_key,
            party.phone.identity_public_key()
        );
        assert_eq!(paired_phone.name, "Jo's iPhone");
        assert_eq!(paired_mac.handshake_hash, paired_phone.handshake_hash);
        assert_eq!(
            confirmation_code(paired_mac.handshake_hash.to_vec()),
            confirmation_code(paired_phone.handshake_hash.to_vec())
        );
    }

    /// Fresh ephemerals make each pairing's handshake hash, and so its code,
    /// different. Six digits can still coincide by chance (about one in a
    /// million), so this compares the hashes; the code derivation itself is
    /// pinned in `confirmation`.
    #[test]
    fn two_pairings_of_the_same_devices_have_different_handshake_hashes() {
        let party = party();
        let (first, _) = pair(&party);
        let (second, _) = pair(&party);
        assert_ne!(first.handshake_hash, second.handshake_hash);
    }

    /// Regression vector: with every key and ephemeral fixed, the handshake is
    /// deterministic. Pinned from this implementation, so it catches a change
    /// to the pattern, prologue, PSK placement, or payload layout; `snow`
    /// itself is checked against the published Noise test vectors.
    #[test]
    fn the_handshake_is_pinned() {
        let party = party();
        let (mut phone, message_1) =
            phone_start_with(&party.phone, &offer(&party), Some(&[6; KEY_LEN])).unwrap();
        let (mut mac, message_2) = mac_respond_with(
            &party.mac,
            &party.psk,
            "Mac",
            &message_1,
            Some(&[7; KEY_LEN]),
        )
        .unwrap();
        let (paired_mac, message_3) = phone.respond("Phone", &message_2).unwrap();
        mac.finish(&message_3).unwrap();
        assert_eq!(
            hex(&paired_mac.handshake_hash),
            "bd9825c8a15e126ec2bba9657131df16443f944e770120544625cbb67710e85c"
        );
        assert_eq!(
            confirmation_code(paired_mac.handshake_hash.to_vec()).unwrap(),
            "112950"
        );
    }

    #[test]
    fn pairing_steps_refuse_frames_of_another_kind() {
        let party = party();
        let (_, mut message_1) = phone_start(&party.phone, &offer(&party)).unwrap();
        message_1[0] = FrameKind::SessionRequest as u8;
        assert_eq!(
            mac_respond(&party.mac, &party.psk, "Mac", &message_1).err(),
            Some(CryptoError::UnexpectedFrame)
        );
    }

    /// Both waiting states survive a frame of another type and complete when
    /// the real message arrives.
    #[test]
    fn a_stray_frame_leaves_a_pairing_step_waiting() {
        let party = party();
        let stray = frame::tagged(FrameKind::Record, b"stray");
        let (mut phone, message_1) = phone_start(&party.phone, &offer(&party)).unwrap();
        let (mut mac, message_2) = mac_respond(&party.mac, &party.psk, "Mac", &message_1).unwrap();
        assert_eq!(
            phone.respond("Phone", &stray).err(),
            Some(CryptoError::UnexpectedFrame)
        );
        let (_, message_3) = phone.respond("Phone", &message_2).unwrap();
        assert_eq!(mac.finish(&stray).err(), Some(CryptoError::UnexpectedFrame));
        assert!(mac.finish(&message_3).is_ok());
        assert_eq!(
            mac.finish(&message_3).err(),
            Some(CryptoError::HandshakeFailed)
        );
    }

    #[test]
    fn a_wrong_psk_fails_the_handshake() {
        let party = party();
        let (mut phone, message_1) = phone_start(&party.phone, &offer(&party)).unwrap();
        let (_, message_2) = mac_respond(&party.mac, &[9; PSK_LEN], "Mac", &message_1).unwrap();
        assert_eq!(
            phone.respond("Phone", &message_2).err(),
            Some(CryptoError::HandshakeFailed)
        );
    }

    #[test]
    fn a_mac_other_than_the_one_in_the_qr_code_is_refused() {
        let party = party();
        let impostor = DeviceKeys::from_secrets(&[8; KEY_LEN], &[8; KEY_LEN]).unwrap();
        let (mut phone, message_1) = phone_start(&party.phone, &offer(&party)).unwrap();
        let (_, message_2) = mac_respond(&impostor, &party.psk, "Mac", &message_1).unwrap();
        assert_eq!(
            phone.respond("Phone", &message_2).err(),
            Some(CryptoError::UnexpectedPeerKey)
        );
    }

    #[test]
    fn a_tampered_message_fails_the_handshake() {
        let party = party();
        let (mut phone, message_1) = phone_start(&party.phone, &offer(&party)).unwrap();
        let (_, mut message_2) = mac_respond(&party.mac, &party.psk, "Mac", &message_1).unwrap();
        let last = message_2.len() - 1;
        message_2[last] ^= 1;
        assert_eq!(
            phone.respond("Phone", &message_2).err(),
            Some(CryptoError::HandshakeFailed)
        );

        let (mut phone, message_1) = phone_start(&party.phone, &offer(&party)).unwrap();
        let (mut mac, message_2) = mac_respond(&party.mac, &party.psk, "Mac", &message_1).unwrap();
        let (_, mut message_3) = phone.respond("Phone", &message_2).unwrap();
        message_3[10] ^= 1;
        assert_eq!(
            mac.finish(&message_3).err(),
            Some(CryptoError::HandshakeFailed)
        );
    }

    /// A phone that signs for a Noise key other than the one it handshook
    /// with — someone else's — is refused.
    #[test]
    fn a_signature_binding_a_different_noise_key_is_refused() {
        let party = party();
        let (mut phone, message_1) = phone_start(&party.phone, &offer(&party)).unwrap();
        let (mut mac, message_2) = mac_respond(&party.mac, &party.psk, "Mac", &message_1).unwrap();
        let (_, message_3) = phone
            .respond_with(&message_2, |identity, hash, _| {
                message_3_payload(identity, hash, &[0xAA; KEY_LEN], "Phone")
            })
            .unwrap();
        assert_eq!(
            mac.finish(&message_3).err(),
            Some(CryptoError::InvalidSignature)
        );
    }

    #[test]
    fn a_signature_over_a_different_handshake_is_refused() {
        let party = party();
        let (mut phone, message_1) = phone_start(&party.phone, &offer(&party)).unwrap();
        let (mut mac, message_2) = mac_respond(&party.mac, &party.psk, "Mac", &message_1).unwrap();
        let (_, message_3) = phone
            .respond_with(&message_2, |identity, _, noise_key| {
                message_3_payload(identity, &[0; HANDSHAKE_HASH_LEN], noise_key, "Phone")
            })
            .unwrap();
        assert_eq!(
            mac.finish(&message_3).err(),
            Some(CryptoError::InvalidSignature)
        );
    }

    #[test]
    fn names_are_cut_to_the_limit_at_a_character_boundary() {
        let mut out = Vec::new();
        encode_name(&"é".repeat(40), &mut out);
        assert_eq!(usize::from(out[0]), 64);
        let (name, rest) = decode_name(&out).unwrap();
        assert_eq!(name, "é".repeat(32));
        assert!(rest.is_empty());
    }

    #[test]
    fn decoded_names_lose_control_characters() {
        let mut out = Vec::new();
        encode_name("Jo\u{1b}[2J's\n iPhone\u{7}", &mut out);
        assert_eq!(decode_name(&out).unwrap().0, "Jo[2J's iPhone");
    }

    #[test]
    fn decoded_names_lose_reordering_and_invisible_characters() {
        let mut out = Vec::new();
        encode_name(
            "Jo\u{202E}enohPi\u{202C}\u{200B}\u{2066}x\u{2069}\u{FEFF}",
            &mut out,
        );
        assert_eq!(decode_name(&out).unwrap().0, "JoenohPix");
    }

    /// U+2028 and U+2029 are not `Cc`, but text views break lines on them, so
    /// a hostile name could otherwise put a fake line beside the real code.
    #[test]
    fn decoded_names_lose_line_and_paragraph_separators() {
        let mut out = Vec::new();
        encode_name("Jo's Mac\u{2028}Verified\u{2029}code 000000", &mut out);
        assert_eq!(decode_name(&out).unwrap().0, "Jo's MacVerifiedcode 000000");
    }

    #[test]
    fn decoded_names_keep_zero_width_joiners() {
        let mut out = Vec::new();
        encode_name("Jo's \u{1F469}\u{200D}\u{1F4BB}", &mut out);
        assert_eq!(
            decode_name(&out).unwrap().0,
            "Jo's \u{1F469}\u{200D}\u{1F4BB}"
        );
    }

    #[test]
    fn malformed_payloads_are_rejected() {
        let mut valid_2 = vec![PAYLOAD_VERSION];
        encode_name("Mac", &mut valid_2);
        assert_eq!(decode_message_2(&valid_2), Ok("Mac".to_owned()));

        let mut wrong_version = valid_2.clone();
        wrong_version[0] = 2;
        let mut trailing = valid_2.clone();
        trailing.push(0);
        let short = valid_2[..valid_2.len() - 1].to_vec();
        let mut over_long = vec![PAYLOAD_VERSION, 65];
        over_long.extend_from_slice(&[b'a'; 65]);
        let invalid_utf8 = vec![PAYLOAD_VERSION, 2, 0xC3, 0x28];
        for payload in [
            vec![],
            wrong_version,
            trailing,
            short,
            over_long,
            invalid_utf8,
        ] {
            assert_eq!(
                decode_message_2(&payload),
                Err(CryptoError::InvalidPayload),
                "{payload:?}"
            );
        }

        let identity = SigningKey::from_bytes(&[2; KEY_LEN]);
        let valid_3 =
            message_3_payload(&identity, &[0; HANDSHAKE_HASH_LEN], &[1; KEY_LEN], "Phone");
        assert!(decode_message_3(&valid_3).is_ok());
        let mut trailing = valid_3.clone();
        trailing.push(0);
        let truncated_signature = valid_3[..1 + KEY_LEN + 10].to_vec();
        let mut wrong_version = valid_3.clone();
        wrong_version[0] = 0;
        for payload in [trailing, truncated_signature, wrong_version] {
            assert_eq!(
                decode_message_3(&payload).err(),
                Some(CryptoError::InvalidPayload)
            );
        }
    }
}
