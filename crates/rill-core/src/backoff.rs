//! Reconnect delays: exponential backoff with jitter, and a tiny random number generator.

use std::time::Duration;

/// Exponential backoff with "equal jitter": each delay is random in [d/2, d].
#[derive(Debug, Clone)]
pub struct Backoff {
    base: Duration,
    max: Duration,
    attempt: u32,
    rng: SmallRng,
}

impl Backoff {
    /// Starts at `base`, doubling up to `max`.
    pub fn new(base: Duration, max: Duration) -> Backoff {
        Backoff {
            base,
            max,
            attempt: 0,
            rng: SmallRng::from_time(),
        }
    }

    /// The default for chat connections: 1 s doubling up to 60 s.
    pub fn chat() -> Backoff {
        Backoff::new(Duration::from_secs(1), Duration::from_secs(60))
    }

    /// Attempts made since the last reset.
    pub fn attempt(&self) -> u32 {
        self.attempt
    }

    /// The delay before the next attempt.
    pub fn next_delay(&mut self) -> Duration {
        let exp = self.attempt.min(16);
        self.attempt = self.attempt.saturating_add(1);
        let full = self.base.saturating_mul(1 << exp).min(self.max);
        let half = full / 2;
        let extra_ms = self.rng.below(half.as_millis() as u64 + 1);
        half + Duration::from_millis(extra_ms)
    }

    /// Call after a connection has stayed up, so the next failure starts small again.
    pub fn reset(&mut self) {
        self.attempt = 0;
    }
}

/// A small, fast, non-cryptographic random number generator (xorshift64*).
#[derive(Debug, Clone)]
pub struct SmallRng(u64);

impl SmallRng {
    /// A generator with a fixed seed, for repeatable output.
    pub fn seeded(seed: u64) -> SmallRng {
        SmallRng(seed.max(1))
    }

    /// A generator seeded from the clock.
    pub fn from_time() -> SmallRng {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(1, |d| d.as_nanos() as u64);
        SmallRng::seeded(nanos ^ 0x9E37_79B9_7F4A_7C15)
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// A number in `0..n` (0 when n is 0).
    pub fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next_u64() % n }
    }

    /// A float in `[0, 1)`.
    pub fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// A random element of a non-empty slice.
    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len() as u64) as usize]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delays_grow_stay_in_bounds_and_reset() {
        let mut b = Backoff::new(Duration::from_secs(1), Duration::from_secs(8));
        let expected_max = [1, 2, 4, 8, 8, 8];
        for max in expected_max {
            let d = b.next_delay();
            let max = Duration::from_secs(max);
            assert!(d >= max / 2 && d <= max, "{d:?} not within {max:?}");
        }
        b.reset();
        assert!(b.next_delay() <= Duration::from_secs(1));
    }

    #[test]
    fn rng_is_repeatable_and_bounded() {
        let mut a = SmallRng::seeded(42);
        let mut b = SmallRng::seeded(42);
        for _ in 0..100 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
        for _ in 0..100 {
            assert!(a.below(7) < 7);
            let u = a.unit();
            assert!((0.0..1.0).contains(&u));
        }
    }
}
