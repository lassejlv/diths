//! Wall-clock timing for reports. `wasm32-unknown-unknown` has no clock (`Instant::now` panics
//! there), so on that target nothing is timed and the elapsed time reads as zero.

use std::time::Instant;

/// The current instant, or `None` on a target without a clock.
pub(crate) fn start() -> Option<Instant> {
    (!cfg!(all(target_arch = "wasm32", target_os = "unknown"))).then(Instant::now)
}

/// Milliseconds since [`start`], or 0 when nothing was timed.
pub(crate) fn elapsed_ms(started: Option<Instant>) -> u128 {
    started.map_or(0, |s| s.elapsed().as_millis())
}
