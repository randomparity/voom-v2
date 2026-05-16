use time::OffsetDateTime;

/// Format an `OffsetDateTime` as ISO-8601, falling back to the unix
/// timestamp string when the formatter rejects the value (e.g. extreme
/// years). Used in every envelope that surfaces `schema_init_at` so the
/// CLI and API can't drift to different fallback policies.
#[must_use]
pub fn format_iso8601(t: OffsetDateTime) -> String {
    t.format(&time::format_description::well_known::Iso8601::DEFAULT)
        .unwrap_or_else(|_| t.unix_timestamp().to_string())
}

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
#[path = "clock_test.rs"]
mod tests;
