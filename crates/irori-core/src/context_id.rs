//! New context ids: ULIDs from the injected clock plus 80 random bits.

use std::sync::atomic::{AtomicU64, Ordering};

use irori_types::{ContextId, Timestamp};

const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

pub(crate) fn new_context_id(now: Timestamp) -> ContextId {
    let millis = now.as_jiff().as_millisecond().clamp(0, (1 << 48) - 1) as u128;
    let value = (millis << 80) | random_80_bits();
    let text: String = (0..26)
        .map(|i| CROCKFORD[((value >> (125 - 5 * i)) & 0x1F) as usize] as char)
        .collect();
    ContextId::try_from(text).expect("a 48-bit time and 80 random bits always form a valid ULID")
}

fn random_80_bits() -> u128 {
    let mut bytes = [0u8; 16];
    if getrandom::fill(&mut bytes[6..]).is_err() {
        // No OS randomness (shouldn't happen on Linux or macOS): stay unique within this process.
        static COUNTER: AtomicU64 = AtomicU64::new(1);
        bytes[8..].copy_from_slice(&COUNTER.fetch_add(1, Ordering::Relaxed).to_be_bytes());
    }
    u128::from_be_bytes(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_valid_distinct_and_sort_by_time() {
        let at = |ms: i64| {
            Timestamp::from_jiff(jiff::Timestamp::from_millisecond(ms).expect("in range"))
        };
        let a = new_context_id(at(1_000));
        let b = new_context_id(at(1_000));
        let c = new_context_id(at(2_000));
        assert_ne!(a, b);
        assert!(a.as_str()[..10] < c.as_str()[..10]);
        assert_eq!(a.as_str().len(), 26);
    }
}
