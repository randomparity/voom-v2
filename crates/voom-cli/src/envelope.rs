use std::io::{self, Write};

use serde::Serialize;

pub const SCHEMA_VERSION: &str = "0";

/// Host-only diagnostics block; emitted by CLI, never by API.
#[derive(Debug, Clone, Serialize)]
pub struct Local {
    pub db_url: String,
    pub config_path: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ErrorBody {
    pub code: &'static str,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Envelope<T: Serialize> {
    pub schema_version: &'static str,
    pub command: &'static str,
    pub status: Status,
    pub data: Option<T>,
    /// Keyset continuation token for paged list commands (ADR 0031): the `id`
    /// to feed back as `--after-id` for the next page. Present only when the
    /// page was full and more rows may exist; omitted otherwise (including on
    /// every non-list command), so its absence is the end-of-stream signal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local: Option<Local>,
    pub warnings: Vec<String>,
    pub error: Option<ErrorBody>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Ok,
    Error,
}

/// Emit a successful envelope as a single JSON object to stdout, followed by a newline.
pub fn emit_ok<T: Serialize>(
    command: &'static str,
    data: T,
    local: Option<Local>,
    warnings: Vec<String>,
) -> io::Result<()> {
    let env = Envelope {
        schema_version: SCHEMA_VERSION,
        command,
        status: Status::Ok,
        data: Some(data),
        next_cursor: None,
        local,
        warnings,
        error: None,
    };
    write_json(&env)
}

/// Emit a successful envelope for a paged list command, carrying the keyset
/// continuation token (ADR 0031). `next_cursor` is `Some(id)` when the page was
/// full and more rows may exist, `None` at end of stream.
pub fn emit_ok_page<T: Serialize>(
    command: &'static str,
    data: T,
    next_cursor: Option<u64>,
    local: Option<Local>,
    warnings: Vec<String>,
) -> io::Result<()> {
    let env = Envelope {
        schema_version: SCHEMA_VERSION,
        command,
        status: Status::Ok,
        data: Some(data),
        next_cursor,
        local,
        warnings,
        error: None,
    };
    write_json(&env)
}

/// Emit an error envelope to stdout.
pub fn emit_err(
    command: &'static str,
    code: &'static str,
    message: String,
    hint: Option<String>,
    local: Option<Local>,
) -> io::Result<()> {
    let env: Envelope<()> = Envelope {
        schema_version: SCHEMA_VERSION,
        command,
        status: Status::Error,
        data: None,
        next_cursor: None,
        local,
        warnings: Vec::new(),
        error: Some(ErrorBody {
            code,
            message,
            hint,
        }),
    };
    write_json(&env)
}

pub fn emit_err_with_data<T: Serialize>(
    command: &'static str,
    data: T,
    code: &'static str,
    message: String,
    hint: Option<String>,
    local: Option<Local>,
) -> io::Result<()> {
    emit_err_with_data_and_warnings(command, data, code, message, hint, local, Vec::new())
}

pub fn emit_err_with_data_and_warnings<T: Serialize>(
    command: &'static str,
    data: T,
    code: &'static str,
    message: String,
    hint: Option<String>,
    local: Option<Local>,
    warnings: Vec<String>,
) -> io::Result<()> {
    let env = Envelope {
        schema_version: SCHEMA_VERSION,
        command,
        status: Status::Error,
        data: Some(data),
        next_cursor: None,
        local,
        warnings,
        error: Some(ErrorBody {
            code,
            message,
            hint,
        }),
    };
    write_json(&env)
}

fn write_json<T: Serialize>(value: &T) -> io::Result<()> {
    let s = serde_json::to_string(value).map_err(io::Error::other)?;
    let mut out = io::stdout().lock();
    writeln!(out, "{s}")?;
    out.flush()
}

#[cfg(test)]
#[path = "envelope_test.rs"]
mod tests;
