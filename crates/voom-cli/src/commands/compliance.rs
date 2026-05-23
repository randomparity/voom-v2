use std::io;

use voom_control_plane::ControlPlane;
use voom_control_plane::cases::compliance::{
    ComplianceApplyData, ComplianceExecuteData, ComplianceReportData,
};
use voom_core::{PolicyInputSetId, PolicyVersionId};

use crate::envelope::{Local, emit_err, emit_err_with_data, emit_ok};

pub type ReportData = ComplianceReportData;
pub type ApplyData = ComplianceApplyData;
pub type ExecuteData = ComplianceExecuteData;

pub async fn report(
    database_url: &str,
    local: Local,
    policy_version_id: u64,
    input_set_id: u64,
) -> io::Result<i32> {
    let cp = match ControlPlane::open(database_url).await {
        Ok(cp) => cp,
        Err(err) => {
            emit_err("compliance", err.code(), err.to_string(), None, Some(local))?;
            return Ok(2);
        }
    };
    match cp
        .generate_compliance_report(
            PolicyVersionId(policy_version_id),
            PolicyInputSetId(input_set_id),
        )
        .await
    {
        Ok(data) => emit_ok("compliance", data, Some(local), Vec::new()).map(|()| 0),
        Err(err) => {
            emit_err("compliance", err.code(), err.to_string(), None, Some(local))?;
            Ok(2)
        }
    }
}

pub async fn apply(
    database_url: &str,
    local: Local,
    policy_version_id: u64,
    input_set_id: u64,
) -> io::Result<i32> {
    let cp = match ControlPlane::open(database_url).await {
        Ok(cp) => cp,
        Err(err) => {
            emit_err("compliance", err.code(), err.to_string(), None, Some(local))?;
            return Ok(2);
        }
    };
    match cp
        .apply_compliance_report(
            PolicyVersionId(policy_version_id),
            PolicyInputSetId(input_set_id),
        )
        .await
    {
        Ok(data) => emit_ok("compliance", data, Some(local), Vec::new()).map(|()| 0),
        Err(err) => {
            emit_err("compliance", err.code(), err.to_string(), None, Some(local))?;
            Ok(2)
        }
    }
}

pub async fn execute(
    database_url: &str,
    local: Local,
    policy_version_id: u64,
    input_set_id: u64,
) -> io::Result<i32> {
    let cp = match ControlPlane::open(database_url).await {
        Ok(cp) => cp,
        Err(err) => {
            emit_err("compliance", err.code(), err.to_string(), None, Some(local))?;
            return Ok(2);
        }
    };
    match cp
        .execute_compliance_policy(
            PolicyVersionId(policy_version_id),
            PolicyInputSetId(input_set_id),
        )
        .await
    {
        Ok(data) => emit_ok("compliance", data, Some(local), Vec::new()).map(|()| 0),
        Err(err) => {
            if let Some(partial) = err.partial {
                emit_err_with_data(
                    "compliance",
                    partial,
                    err.source.code(),
                    err.source.to_string(),
                    None,
                    Some(local),
                )?;
            } else {
                emit_err(
                    "compliance",
                    err.source.code(),
                    err.source.to_string(),
                    None,
                    Some(local),
                )?;
            }
            Ok(2)
        }
    }
}

#[cfg(test)]
#[path = "compliance_test.rs"]
mod tests;
