use super::*;

#[test]
fn system_clock_returns_recent_timestamp() {
    let before = OffsetDateTime::now_utc();
    let now = SystemClock.now();
    let after = OffsetDateTime::now_utc();
    assert!(now >= before && now <= after);
}
