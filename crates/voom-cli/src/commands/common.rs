use std::io;

use voom_control_plane::ControlPlane;
use voom_core::VoomError;

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
                None,
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
