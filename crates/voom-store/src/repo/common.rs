//! Shared helpers used by every repository module in this crate.
//! Internal to `voom-store::repo` — not part of the public API.

use serde::Serialize;
use time::OffsetDateTime;
use voom_core::VoomError;

pub(crate) fn i64_from_u64(v: u64, field: impl std::fmt::Display) -> Result<i64, VoomError> {
    i64::try_from(v)
        .map_err(|error| VoomError::database_context(format!("{field}: i64 conv from {v}"), error))
}

pub(crate) fn u64_from_i64(v: i64, field: impl std::fmt::Display) -> Result<u64, VoomError> {
    u64::try_from(v)
        .map_err(|error| VoomError::database_context(format!("{field}: u64 conv from {v}"), error))
}

pub(crate) fn u32_from_i64(v: i64) -> Result<u32, VoomError> {
    u32::try_from(v).map_err(|e| VoomError::database_context(format!("u32 conv from {v}"), e))
}

pub(crate) fn iso8601(t: OffsetDateTime) -> Result<String, VoomError> {
    t.format(&time::format_description::well_known::Iso8601::DEFAULT)
        .map_err(|e| VoomError::Internal(format!("format iso8601: {e}")))
}

pub(crate) fn parse_iso8601(s: &str) -> Result<OffsetDateTime, VoomError> {
    OffsetDateTime::parse(s, &time::format_description::well_known::Iso8601::DEFAULT)
        .map_err(|e| VoomError::database_context(format!("parse iso8601 {s:?}"), e))
}

pub(crate) fn serialize_json<T: Serialize + ?Sized>(
    v: &T,
    field: &str,
) -> Result<String, VoomError> {
    serde_json::to_string(v).map_err(|e| VoomError::Internal(format!("serialize {field}: {e}")))
}

pub(crate) fn map_row_err(table: &'static str, e: sqlx::Error) -> VoomError {
    VoomError::database_context(format!("{table} row decode"), e)
}
