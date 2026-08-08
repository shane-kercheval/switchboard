//! Project-local message pins.
//!
//! Pins are mutable UI annotations, not conversation history, so they live in
//! their own small rewritten file rather than the append-only journal. The key
//! is an opaque frontend-owned message identity; core only guarantees ordered,
//! durable storage.

use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::io::{read_jsonl, write_jsonl};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MessagePin {
    pub key: String,
    pub pinned_at: DateTime<Utc>,
}

pub fn read_pins(path: &Path) -> Result<Vec<MessagePin>> {
    read_jsonl(path)
}

pub fn write_pins(path: &Path, pins: &[MessagePin]) -> Result<()> {
    write_jsonl(path, pins)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn missing_is_empty_and_rewrite_round_trips_order() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("pins.jsonl");
        assert!(read_pins(&path).unwrap().is_empty());

        let pins = vec![
            MessagePin {
                key: "user:send:a".to_owned(),
                pinned_at: "2026-08-07T12:00:00Z".parse().unwrap(),
            },
            MessagePin {
                key: "agent:send:a:b".to_owned(),
                pinned_at: "2026-08-07T12:01:00Z".parse().unwrap(),
            },
        ];
        write_pins(&path, &pins).unwrap();
        assert_eq!(read_pins(&path).unwrap(), pins);

        write_pins(&path, &pins[1..]).unwrap();
        assert_eq!(read_pins(&path).unwrap(), pins[1..]);
        assert!(!path.with_extension("jsonl.tmp").exists());
    }
}
