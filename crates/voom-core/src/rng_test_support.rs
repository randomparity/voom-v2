//! Test-only RNGs. Gated behind the `test-support` feature, mirroring
//! the `clock_test_support` pattern.
//!
//! `FrozenRng` returns a fixed `u32` from every call — wire it into
//! `TicketRepo::default_backoff` to land the jitter at the floor (`0`)
//! or ceiling (`u32::MAX`) of the computed window deterministically.
//! `SeededRng` wraps `StdRng::seed_from_u64` for property-style
//! repeatability across runs.

use rand::RngCore;
use rand::SeedableRng;
use rand::rngs::StdRng;

/// Deterministic RNG that returns the configured `u32` from every
/// `next_u32` call (and the same bits doubled for `next_u64`). Use
/// `FrozenRng::new(0)` for the jitter floor and `FrozenRng::new(u32::MAX)`
/// for the ceiling in tests that assert exact `next_eligible_at` values.
#[derive(Debug, Clone, Copy)]
pub struct FrozenRng {
    value: u32,
}

impl FrozenRng {
    #[must_use]
    pub const fn new(value: u32) -> Self {
        Self { value }
    }
}

impl RngCore for FrozenRng {
    fn next_u32(&mut self) -> u32 {
        self.value
    }

    fn next_u64(&mut self) -> u64 {
        u64::from(self.value) << 32 | u64::from(self.value)
    }

    fn fill_bytes(&mut self, dst: &mut [u8]) {
        for chunk in dst.chunks_mut(4) {
            let bytes = self.value.to_le_bytes();
            for (slot, byte) in chunk.iter_mut().zip(bytes.iter()) {
                *slot = *byte;
            }
        }
    }
}

/// Seedable RNG for property-style tests that need reproducibility
/// across runs without locking the exact jitter value. Thin wrapper
/// over `rand::rngs::StdRng::seed_from_u64`.
#[derive(Debug, Clone)]
pub struct SeededRng {
    inner: StdRng,
}

impl SeededRng {
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self {
            inner: StdRng::seed_from_u64(seed),
        }
    }
}

impl RngCore for SeededRng {
    fn next_u32(&mut self) -> u32 {
        self.inner.next_u32()
    }

    fn next_u64(&mut self) -> u64 {
        self.inner.next_u64()
    }

    fn fill_bytes(&mut self, dst: &mut [u8]) {
        self.inner.fill_bytes(dst);
    }
}

#[cfg(test)]
#[path = "rng_test_support_test.rs"]
mod tests;
