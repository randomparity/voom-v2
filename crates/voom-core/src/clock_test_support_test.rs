use super::*;
use time::Duration;
use time::OffsetDateTime;

use crate::clock::Clock;

#[test]
fn frozen_clock_returns_same_time_on_every_call() {
    let t = OffsetDateTime::UNIX_EPOCH + Duration::seconds(1_000);
    let clock = FrozenClock::new(t);
    assert_eq!(clock.now(), t);
    assert_eq!(clock.now(), t);
}

#[test]
fn manual_clock_advance_shifts_now() {
    let t0 = OffsetDateTime::UNIX_EPOCH;
    let clock = ManualClock::new(t0);
    assert_eq!(clock.now(), t0);
    clock.advance(Duration::seconds(60));
    assert_eq!(clock.now(), t0 + Duration::seconds(60));
}

#[test]
fn manual_clock_set_replaces_now() {
    let t0 = OffsetDateTime::UNIX_EPOCH;
    let t1 = OffsetDateTime::UNIX_EPOCH + Duration::days(7);
    let clock = ManualClock::new(t0);
    clock.set(t1);
    assert_eq!(clock.now(), t1);
}
