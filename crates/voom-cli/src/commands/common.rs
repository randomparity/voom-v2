use std::io;

use voom_control_plane::ControlPlane;
use voom_core::{ErrorCode, VoomError};

use crate::commands::health::voom_error_hint;
use crate::envelope::{Local, emit_err};

pub async fn open_control_plane(
    command: &'static str,
    database_url: &str,
    local: &Local,
) -> io::Result<Result<ControlPlane, i32>> {
    match ControlPlane::open(database_url).await {
        Ok(cp) => Ok(Ok(cp)),
        Err(err) => {
            emit_err(
                command,
                err.code(),
                err.to_string(),
                open_error_hint(&err),
                Some(local.clone()),
            )?;
            Ok(Err(2))
        }
    }
}

pub fn emit_voom_error(command: &'static str, err: &VoomError, local: Local) -> io::Result<i32> {
    emit_err(command, err.code(), err.to_string(), None, Some(local))?;
    Ok(2)
}

/// Keyset continuation token for a page of rows (ADR 0031). Returns the `id` of
/// the last row when the page is full (`rows.len() >= limit` and `limit > 0`),
/// signalling more rows may exist; `None` at end of stream. `id_of` extracts the
/// stable primary id an agent feeds back as `--after-id`.
pub fn next_cursor<T>(rows: &[T], limit: u32, id_of: impl Fn(&T) -> u64) -> Option<u64> {
    if limit > 0 && rows.len() as u64 >= u64::from(limit) {
        rows.last().map(id_of)
    } else {
        None
    }
}

fn open_error_hint(err: &VoomError) -> Option<String> {
    match err.error_code() {
        ErrorCode::DbUninitialized | ErrorCode::DbUnreachable => voom_error_hint(err),
        _ => None,
    }
}
