use time::OffsetDateTime;

/// Wall-clock abstraction; production uses `SystemClock`, tests inject fakes.
pub trait Clock: Send + Sync {
    fn now(&self) -> OffsetDateTime;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_clock_returns_recent_timestamp() {
        let before = OffsetDateTime::now_utc();
        let now = SystemClock.now();
        let after = OffsetDateTime::now_utc();
        assert!(now >= before && now <= after);
    }
}
