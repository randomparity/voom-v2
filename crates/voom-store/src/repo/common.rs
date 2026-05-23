//! Shared helpers used by every repository module in this crate.
//! Internal to `voom-store::repo` — not part of the public API.

use serde::Serialize;
use time::OffsetDateTime;
use voom_core::VoomError;

#[expect(clippy::cast_possible_wrap, reason = "SQLite rowid fits i64")]
pub(crate) const fn i64_from_u64(v: u64) -> i64 {
    v as i64
}

#[expect(clippy::cast_sign_loss, reason = "SQLite rowid is non-negative")]
pub(crate) const fn u64_from_i64(v: i64) -> u64 {
    v as u64
}

pub(crate) fn u32_from_i64(v: i64) -> Result<u32, VoomError> {
    u32::try_from(v).map_err(|e| VoomError::Database(format!("u32 conv from {v}: {e}")))
}

pub(crate) fn iso8601(t: OffsetDateTime) -> Result<String, VoomError> {
    t.format(&time::format_description::well_known::Iso8601::DEFAULT)
        .map_err(|e| VoomError::Internal(format!("format iso8601: {e}")))
}

pub(crate) fn parse_iso8601(s: &str) -> Result<OffsetDateTime, VoomError> {
    OffsetDateTime::parse(s, &time::format_description::well_known::Iso8601::DEFAULT)
        .map_err(|e| VoomError::Database(format!("parse iso8601 {s:?}: {e}")))
}

pub(crate) fn serialize_json<T: Serialize + ?Sized>(
    v: &T,
    field: &str,
) -> Result<String, VoomError> {
    serde_json::to_string(v).map_err(|e| VoomError::Internal(format!("serialize {field}: {e}")))
}

pub(crate) fn map_row_err(table: &'static str, e: &sqlx::Error) -> VoomError {
    VoomError::Database(format!("{table} row decode: {e}"))
}
