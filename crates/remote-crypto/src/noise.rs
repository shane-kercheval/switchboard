//! What the pairing and session handshakes share: building a `snow` handshake
//! from a device's keys, and reading and writing its messages.

use snow::{Builder, HandshakeState};

use crate::fragment::MAX_RECORD_LEN;
use crate::keys::{DeviceKeys, KEY_LEN};
use crate::{CryptoError, HANDSHAKE_HASH_LEN};

/// How a handshake authenticates the other side beyond its own static key.
#[derive(Clone, Copy)]
pub(crate) enum Peer<'a> {
    /// Pairing: a pre-shared key from the QR code, at this message position.
    Psk { location: u8, key: &'a [u8; 32] },
    /// A session: the peer's static key, pinned at pairing.
    Pinned(&'a [u8; KEY_LEN]),
}

/// A handshake builder for `pattern`. There is deliberately no way to fix the
/// ephemeral key here: `snow` does not gate that override, and a fixed
/// ephemeral in real use would derive every session's keys from one value.
/// Only tests add one, to pin regression vectors.
pub(crate) fn builder<'a>(
    pattern: &str,
    prologue: &'a [u8],
    keys: &'a DeviceKeys,
    peer: Peer<'a>,
) -> Result<Builder<'a>, CryptoError> {
    let params = pattern.parse().map_err(|_| CryptoError::HandshakeFailed)?;
    let builder = Builder::new(params)
        .local_private_key(keys.noise_private_key())
        .and_then(|b| b.prologue(prologue))
        .map_err(|_| CryptoError::HandshakeFailed)?;
    match peer {
        Peer::Psk { location, key } => builder.psk(location, key),
        Peer::Pinned(key) => builder.remote_public_key(key),
    }
    .map_err(|_| CryptoError::HandshakeFailed)
}

pub(crate) fn read(handshake: &mut HandshakeState, message: &[u8]) -> Result<Vec<u8>, CryptoError> {
    // A handshake payload is never longer than its message, and a message
    // longer than the Noise maximum is refused before decryption.
    let mut payload = vec![0u8; message.len().min(MAX_RECORD_LEN)];
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
    let mut message = vec![0u8; MAX_RECORD_LEN];
    let len = handshake
        .write_message(payload, &mut message)
        .map_err(|_| CryptoError::HandshakeFailed)?;
    message.truncate(len);
    message.shrink_to_fit();
    Ok(message)
}

/// Length of the echo a handshake reply starts with.
pub(crate) const ECHO_LEN: usize = KEY_LEN;

/// What a reply repeats so the initiator can tell its own reply from one to
/// an attempt it abandoned: the initiator's ephemeral public key, which
/// starts message 1 in the clear in both handshake patterns and is fresh per
/// attempt. Not authenticated: a relay that changes it only gets the reply
/// ignored, which it could do by dropping the reply.
pub(crate) fn echo_of(message_1: &[u8]) -> Result<[u8; ECHO_LEN], CryptoError> {
    message_1
        .get(..ECHO_LEN)
        .and_then(|echo| <[u8; ECHO_LEN]>::try_from(echo).ok())
        .ok_or(CryptoError::HandshakeFailed)
}

/// A reply's body: the echo, then the handshake message.
pub(crate) fn with_echo(echo: &[u8; ECHO_LEN], message: &[u8]) -> Vec<u8> {
    let mut body = Vec::with_capacity(ECHO_LEN + message.len());
    body.extend_from_slice(echo);
    body.extend_from_slice(message);
    body
}

/// The handshake message in a reply to the attempt that sent `echo`. A reply
/// to any other attempt is `UnexpectedFrame`, so the caller's handshake is
/// untouched and still waiting.
pub(crate) fn strip_echo<'a>(
    echo: &[u8; ECHO_LEN],
    body: &'a [u8],
) -> Result<&'a [u8], CryptoError> {
    body.strip_prefix(echo.as_slice())
        .ok_or(CryptoError::UnexpectedFrame)
}

pub(crate) fn remote_static(handshake: &HandshakeState) -> Result<[u8; KEY_LEN], CryptoError> {
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
