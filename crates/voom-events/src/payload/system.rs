use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

// --- system -----------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SchemaInitializedPayload {
    pub migrations_applied: u32,
    #[serde(with = "time::serde::iso8601")]
    pub schema_init_at: OffsetDateTime,
}

#[cfg(test)]
#[path = "system_test.rs"]
mod tests;
