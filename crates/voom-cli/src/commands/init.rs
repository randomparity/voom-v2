use std::io;

use serde::Serialize;

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
                schema_init_at: report
                    .schema_init_at
                    .format(&time::format_description::well_known::Iso8601::DEFAULT)
                    .unwrap_or_else(|_| report.schema_init_at.unix_timestamp().to_string()),
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
