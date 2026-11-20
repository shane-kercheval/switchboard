//! Switchboard's relay: forwards encrypted frames between a Mac and its paired
//! phones so that neither listens for inbound connections. It reads only the
//! relay-level fields of a message — sender, recipient, pairing token — and
//! never the frames it forwards, which `switchboard-remote-crypto` produces
//! and only the two ends can open. It holds nothing durable: a restart makes
//! every client reconnect, and the Mac's next registration restores what the
//! relay needs.

pub mod config;
mod outbox;
pub mod protocol;
mod server;
mod state;

pub use config::{Config, ConfigError};
pub use server::{RelayHandle, serve};
