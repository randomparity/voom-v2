//! Test-only clocks. Gated behind the `test-support` feature.

use std::sync::Mutex;
use time::{Duration, OffsetDateTime};

use crate::clock::Clock;

/// A clock that always returns the same time.
#[derive(Debug, Clone, Copy)]
pub struct FrozenClock {
    now: OffsetDateTime,
}

impl FrozenClock {
    #[must_use]
    pub fn new(now: OffsetDateTime) -> Self {
        Self { now }
    }
}

impl Clock for FrozenClock {
    fn now(&self) -> OffsetDateTime {
        self.now
    }
}

/// A clock whose time can be advanced or replaced from tests.
#[derive(Debug)]
pub struct ManualClock {
    now: Mutex<OffsetDateTime>,
}

impl ManualClock {
    #[must_use]
    pub fn new(now: OffsetDateTime) -> Self {
        Self {
            now: Mutex::new(now),
        }
    }

    pub fn advance(&self, delta: Duration) {
        let mut guard = self
            .now
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *guard += delta;
    }

    pub fn set(&self, now: OffsetDateTime) {
        let mut guard = self
            .now
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *guard = now;
    }
}

impl Clock for ManualClock {
    fn now(&self) -> OffsetDateTime {
        let guard = self
            .now
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *guard
    }
}

#[cfg(test)]
#[path = "clock_test_support_test.rs"]
mod tests;
