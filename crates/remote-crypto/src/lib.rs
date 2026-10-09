//! Remote-access cryptography shared by the Mac and the iOS app.
//!
//! The iOS app consumes this crate through `UniFFI`, so everything exported here
//! is one implementation for both ends of a handshake.

mod confirmation;
mod error;
// Only a session fragments, so records are always produced under its lock.
pub mod device;
pub mod ffi;
pub(crate) mod fragment;
pub mod frame;
pub mod identity;
pub mod keys;
mod noise;
pub mod pairing;
pub mod phone;
pub mod session;
#[cfg(test)]
mod test_support;

// UniFFI turns a Rust panic into a Swift error only by unwinding it; under
// `panic = "abort"` the same panic kills the app.
#[cfg(all(target_os = "ios", panic = "abort"))]
compile_error!(
    "iOS builds must unwind panics so they reach Swift as errors: build with `--profile ios` (`make ios-crypto`)"
);

pub use confirmation::{HANDSHAKE_HASH_LEN, confirmation_code};
pub use error::CryptoError;
pub use keys::DeviceKeys;

uniffi::setup_scaffolding!();
