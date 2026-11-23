//! Random spread for the account service's waits, so machines never act in step.
//!
//! A deploy of rakumux.com closes every machine's realtime connection at once and may answer
//! every request with an error for a while. Waits worked out the same way on every machine
//! (a doubling from the same start, a `Retry-After`, a reconnect after a fixed time) would then
//! bring the whole fleet back in the same second, again and again. Every wait before retrying
//! a request to rakumux.com, and before connecting again, goes through [`later`] or [`within`].
//!
//! [`later`] never shortens a wait: a `Retry-After` and a backoff's minimum hold.

use std::time::Duration;

/// How much a wait may grow, as a share of itself: up to half again.
const SPREAD: f64 = 0.5;

/// `wait`, or up to half again: a random point in `[wait, 1.5 × wait)`.
pub fn later(wait: Duration) -> Duration {
    later_by(wait, unit())
}

/// A random point in `[low, high)` (`low` when `high` isn't above it).
pub fn within(low: Duration, high: Duration) -> Duration {
    within_by(low, high, unit())
}

/// [`later`] for a given point `u` in `[0, 1)`.
pub(crate) fn later_by(wait: Duration, u: f64) -> Duration {
    wait.saturating_add(wait.mul_f64(SPREAD * clamp(u)))
}

/// [`within`] for a given point `u` in `[0, 1)`.
pub(crate) fn within_by(low: Duration, high: Duration, u: f64) -> Duration {
    match high.checked_sub(low) {
        Some(range) if !range.is_zero() => low.saturating_add(range.mul_f64(clamp(u))),
        _ => low,
    }
}

fn clamp(u: f64) -> f64 {
    if u.is_finite() {
        u.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// A random point in `[0, 1)`; 0 (no spread) when the system has no randomness to give.
#[cfg(not(test))]
fn unit() -> f64 {
    random_unit()
}

/// Tests: no spread unless [`testing::spread`] sets a point, so timing tests stay exact.
#[cfg(test)]
fn unit() -> f64 {
    testing::POINT.with(|point| point.get()).unwrap_or(0.0)
}

#[cfg_attr(test, allow(dead_code))]
fn random_unit() -> f64 {
    let mut bytes = [0u8; 8];
    if getrandom::getrandom(&mut bytes).is_err() {
        return 0.0;
    }
    // 53 random bits: every value exactly representable, strictly below 1.
    (u64::from_le_bytes(bytes) >> 11) as f64 / (1u64 << 53) as f64
}

#[cfg(test)]
pub(crate) mod testing {
    use std::cell::Cell;

    thread_local! {
        pub(super) static POINT: Cell<Option<f64>> = const { Cell::new(None) };
    }

    /// Runs `body` with every spread at point `u` (in `[0, 1)`) on this thread.
    pub fn spread<T>(u: f64, body: impl FnOnce() -> T) -> T {
        let before = POINT.with(|point| point.replace(Some(u)));
        let result = body();
        POINT.with(|point| point.set(before));
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wait_only_grows_and_by_at_most_half() {
        let wait = Duration::from_secs(10);
        assert_eq!(later_by(wait, 0.0), wait);
        assert_eq!(later_by(wait, 0.5), Duration::from_millis(12_500));
        assert!(later_by(wait, 0.999_999) < Duration::from_secs(15));
        assert_eq!(later_by(wait, f64::NAN), wait);
        assert_eq!(later_by(Duration::MAX, 0.9), Duration::MAX);
    }

    #[test]
    fn a_point_within_a_range() {
        let (low, high) = (Duration::from_secs(1), Duration::from_secs(11));
        assert_eq!(within_by(low, high, 0.0), low);
        assert_eq!(within_by(low, high, 0.5), Duration::from_secs(6));
        assert!(within_by(low, high, 0.999_999) < high);
        assert_eq!(within_by(high, low, 0.5), high);
    }

    #[test]
    fn real_randomness_spreads_waits_across_the_range() {
        let wait = Duration::from_secs(60);
        let picks: Vec<Duration> = (0..200).map(|_| later_by(wait, random_unit())).collect();
        assert!(picks
            .iter()
            .all(|pick| *pick >= wait && *pick < wait.mul_f64(1.5)));
        // 200 draws all within a tenth of the range would mean no spread at all.
        let (least, most) = (picks.iter().min().unwrap(), picks.iter().max().unwrap());
        assert!(
            *most - *least > Duration::from_secs(3),
            "{least:?}..{most:?}"
        );
    }
}
