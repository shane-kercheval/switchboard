//! Remote-access cryptography shared by the Mac and the iOS app.
//!
//! The iOS app consumes this crate through `UniFFI`, so everything exported here
//! is one implementation for both ends of a handshake.

mod confirmation;
mod error;
// Only a session fragments, so records are always produced under its lock.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "only the tests call it until the session module does"
    )
)]
pub(crate) mod fragment;
pub mod identity;

// UniFFI turns a Rust panic into a Swift error only by unwinding it; under
// `panic = "abort"` the same panic kills the app.
#[cfg(all(target_os = "ios", panic = "abort"))]
compile_error!(
    "iOS builds must unwind panics so they reach Swift as errors: build with `--profile ios` (`make ios-crypto`)"
);

pub use confirmation::{HANDSHAKE_HASH_LEN, confirmation_code};
pub use error::CryptoError;

uniffi::setup_scaffolding!();
