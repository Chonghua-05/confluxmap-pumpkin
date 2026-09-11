//! Wall-clock time.
//!
//! Two things in the waypoint protocol need a real clock: the creation timestamp
//! that goes into every point's `createdAtEpochMs`, and the token buckets that
//! pace control requests. Both want milliseconds since the Unix epoch, and the
//! plugin API exposes no clock of its own, so `std`'s `SystemTime` is the source.
//!
//! A clock that is unreadable yields `0` rather than a panic. That keeps a
//! misbehaving host from turning a waypoint mutation into a trap inside a plugin
//! callback; the cost is a timestamp of zero, which the wire format accepts.

use std::time::{SystemTime, UNIX_EPOCH};

/// Milliseconds since the Unix epoch, or `0` if the clock could not be read.
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_clock_moves_forward_and_is_plausible() {
        let first = now_ms();
        // Some time after the plugin's own release, and not in the year 100000.
        assert!(first > 1_700_000_000_000, "got {first}");
        assert!(now_ms() >= first, "time must not run backwards");
    }
}
