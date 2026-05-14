//! event_id — deterministic, time-prefixed ULID generation for agent-activity events.
//!
//! Requirement (R1A): re-parsing the same source MUST yield the same event_id.
//! This is the idempotency contract that lets UPSERT semantics work safely — the
//! ingestion pipeline can re-parse the same Claude Code session log on every file
//! change without producing duplicate rows, because the derived event_id is stable.
//!
//! Implementation:
//!   - ULID format (26-char Crockford-base32 string). Chosen over UUIDv7 per the
//!     agent-activity-v1.md §5.1 recommendation ("ULID strongly preferred over
//!     UUIDv4 for natural ordering") and the design doc's Week 1 decision.
//!   - The 48-bit timestamp part = millis-since-epoch of `started_at`. Sortable.
//!   - The 80-bit randomness part = first 10 bytes of
//!     SHA-256(tool || NUL || session_id || NUL || started_at_rfc3339).
//!     NUL delimiters prevent collisions like (tool="ab", id="c") vs (tool="a", id="bc").
//!   - Deterministic: same inputs → bit-identical hash → bit-identical ULID.
//!
//! Performance: SHA-256 of <300 bytes is ~1µs on M-series Mac. Trivial.

use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use ulid::Ulid;

/// Derive a deterministic, time-prefixed event_id from the source's stable
/// identifying tuple. Reparsing the same source MUST yield the same return value.
pub fn derive_event_id(tool: &str, session_id: &str, started_at: DateTime<Utc>) -> String {
    // Hash the identifying tuple with NUL-byte delimiters.
    let mut hasher = Sha256::new();
    hasher.update(tool.as_bytes());
    hasher.update(b"\x00");
    hasher.update(session_id.as_bytes());
    hasher.update(b"\x00");
    hasher.update(started_at.to_rfc3339().as_bytes());
    let hash = hasher.finalize();

    // Pack the first 10 bytes (80 bits) of the hash into a u128 for the
    // ULID randomness field. Big-endian, zero-padded in the high bytes.
    let mut buf = [0u8; 16];
    buf[6..16].copy_from_slice(&hash[..10]);
    let randomness = u128::from_be_bytes(buf);

    // ULID timestamp is 48-bit ms-since-epoch. Saturate negative timestamps
    // (pre-1970) to 0 — those would be data corruption anyway, not valid sessions.
    let ms = started_at.timestamp_millis().max(0) as u64;

    Ulid::from_parts(ms, randomness).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn ts(year: i32, month: u32, day: u32, hour: u32, min: u32, sec: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(year, month, day, hour, min, sec).unwrap()
    }

    #[test]
    fn same_inputs_yield_same_id() {
        // The idempotency contract. This is the most important test.
        let id1 = derive_event_id("claude-code", "sess-abc123", ts(2026, 5, 14, 17, 14, 0));
        let id2 = derive_event_id("claude-code", "sess-abc123", ts(2026, 5, 14, 17, 14, 0));
        assert_eq!(id1, id2, "same inputs must yield same event_id");
    }

    #[test]
    fn different_tool_yields_different_id() {
        let id1 = derive_event_id("claude-code", "sess-abc", ts(2026, 5, 14, 17, 14, 0));
        let id2 = derive_event_id("codex-cli", "sess-abc", ts(2026, 5, 14, 17, 14, 0));
        assert_ne!(id1, id2);
    }

    #[test]
    fn different_session_id_yields_different_id() {
        let id1 = derive_event_id("claude-code", "sess-aaa", ts(2026, 5, 14, 17, 14, 0));
        let id2 = derive_event_id("claude-code", "sess-bbb", ts(2026, 5, 14, 17, 14, 0));
        assert_ne!(id1, id2);
    }

    #[test]
    fn different_timestamp_yields_different_id() {
        let id1 = derive_event_id("claude-code", "sess-abc", ts(2026, 5, 14, 17, 14, 0));
        let id2 = derive_event_id("claude-code", "sess-abc", ts(2026, 5, 14, 17, 14, 1));
        assert_ne!(id1, id2);
    }

    #[test]
    fn null_byte_delimiters_prevent_collision() {
        // (tool="ab", session_id="c") and (tool="a", session_id="bc") must NOT collide.
        // Without NUL delimiters, both would hash the bytes "abc" the same way.
        let id1 = derive_event_id("ab", "c", ts(2026, 5, 14, 17, 14, 0));
        let id2 = derive_event_id("a", "bc", ts(2026, 5, 14, 17, 14, 0));
        assert_ne!(id1, id2);
    }

    #[test]
    fn output_is_valid_ulid_format() {
        // ULIDs are exactly 26 chars of Crockford base32. Strict format check
        // catches encoder regressions if we ever swap implementations.
        let id = derive_event_id("claude-code", "sess-abc", ts(2026, 5, 14, 17, 14, 0));
        assert_eq!(id.len(), 26, "ULID strings are exactly 26 chars");
        assert!(id.chars().all(|c| c.is_ascii_alphanumeric()), "ULIDs are alphanumeric");
        // Round-trip parse must succeed
        let parsed = Ulid::from_string(&id).expect("derived id must be valid ULID");
        assert_eq!(parsed.to_string(), id);
    }

    #[test]
    fn ids_sort_by_timestamp() {
        // The whole point of using ULID over UUIDv4: time-prefixed string sort
        // is chronological. Verify this property holds end-to-end.
        let early = derive_event_id("claude-code", "sess-z", ts(2026, 1, 1, 0, 0, 0));
        let late = derive_event_id("claude-code", "sess-a", ts(2026, 12, 31, 23, 59, 59));
        assert!(early < late, "earlier timestamp must sort first: {} < {}", early, late);
    }
}
