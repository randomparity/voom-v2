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
        local,
        warnings: Vec::new(),
        error: Some(ErrorBody { code, message, hint }),
    };
    write_json(&env)
}

#[expect(
    clippy::print_stdout,
    reason = "envelope writer is the one place CLI output is allowed to reach stdout"
)]
fn write_json<T: Serialize>(value: &T) -> io::Result<()> {
    let s = serde_json::to_string(value).map_err(io::Error::other)?;
    let mut out = io::stdout().lock();
    writeln!(out, "{s}")?;
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Serialize;

    #[derive(Serialize)]
    struct Hello {
        msg: &'static str,
    }

    #[test]
    fn ok_envelope_includes_status_ok() {
        let env = Envelope {
            schema_version: SCHEMA_VERSION,
            command: "test",
            status: Status::Ok,
            data: Some(Hello { msg: "hi" }),
            local: None,
            warnings: Vec::new(),
            error: None,
        };
        let json = serde_json::to_value(&env).unwrap();
        assert_eq!(json["status"], "ok");
        assert_eq!(json["data"]["msg"], "hi");
        assert!(json.get("local").is_none());
    }

    #[test]
    fn local_block_serializes_when_present() {
        let env = Envelope::<()> {
            schema_version: SCHEMA_VERSION,
            command: "test",
            status: Status::Ok,
            data: None,
            local: Some(Local {
                db_url: "sqlite::memory:".into(),
                config_path: "/etc/voom".into(),
            }),
            warnings: Vec::new(),
            error: None,
        };
        let json = serde_json::to_value(&env).unwrap();
        assert_eq!(json["local"]["db_url"], "sqlite::memory:");
    }

    #[test]
    fn error_envelope_omits_data() {
        let env: Envelope<()> = Envelope {
            schema_version: SCHEMA_VERSION,
            command: "test",
            status: Status::Error,
            data: None,
            local: None,
            warnings: Vec::new(),
            error: Some(ErrorBody {
                code: "DB_UNREACHABLE",
                message: "boom".into(),
                hint: None,
            }),
        };
        let json = serde_json::to_value(&env).unwrap();
        assert_eq!(json["status"], "error");
        assert!(json["data"].is_null());
        assert_eq!(json["error"]["code"], "DB_UNREACHABLE");
    }
}
