//! Remote-access cryptography shared by the Mac and the iOS app.
//!
//! The iOS app consumes this crate through `UniFFI`, so everything exported here
//! is one implementation for both ends of a handshake.

mod confirmation;

pub use confirmation::{CryptoError, HANDSHAKE_HASH_LEN, confirmation_code};

uniffi::setup_scaffolding!();
