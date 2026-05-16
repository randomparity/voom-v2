use std::io;

use serde::Serialize;
use voom_core::format_iso8601;

use crate::envelope::{Local, emit_err, emit_ok};

#[derive(Debug, Serialize)]
pub struct InitData {
    pub migrations_applied: u32,
    pub schema_init_at: String,
    pub already_initialized: bool,
}

pub async fn run(database_url: &str, local: Local) -> io::Result<i32> {
    match voom_store::init(database_url).await {
        Ok(report) => {
            let data = InitData {
                migrations_applied: report.migrations_applied,
                schema_init_at: format_iso8601(report.schema_init_at),
                already_initialized: report.already_initialized,
            };
            emit_ok("init", data, Some(local), Vec::new()).map(|()| 0)
        }
        Err(err) => {
            emit_err("init", err.code(), err.to_string(), None, Some(local))?;
            Ok(2)
        }
    }
}
