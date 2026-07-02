use std::io;

use voom_control_plane::policy::{
    ComplianceApplyData, ComplianceExecuteData, ComplianceExecutionOptions, ComplianceReportData,
    ComplianceRunReportData,
};
use voom_core::{ErrorCode, JobId, PolicyInputSetId, PolicyVersionId};

use crate::commands::common::open_control_plane;
use crate::envelope::{Local, emit_err, emit_err_with_data, emit_ok};

pub type ReportData = ComplianceReportData;
pub type ApplyData = ComplianceApplyData;
pub type ExecuteData = ComplianceExecuteData;
pub type RunReportData = ComplianceRunReportData;

/// Parsed `compliance report` mode after argument validation.
enum ReportMode {
    Preview {
        policy_version_id: u64,
        input_set_id: u64,
    },
    Run {
        job_id: u64,
    },
}

/// Validate the `report` argument combination. clap's `requires` /
/// `conflicts_with` attributes already reject `--job-id` alongside a preview arg
/// and a lone preview arg; this catches the "none supplied" case clap cannot
/// express, returning a `BAD_ARGS` message.
fn parse_report_mode(
    policy_version_id: Option<u64>,
    input_set_id: Option<u64>,
    job_id: Option<u64>,
) -> Result<ReportMode, String> {
    match (policy_version_id, input_set_id, job_id) {
        (Some(policy_version_id), Some(input_set_id), None) => Ok(ReportMode::Preview {
            policy_version_id,
            input_set_id,
        }),
        (None, None, Some(job_id)) => Ok(ReportMode::Run { job_id }),
        _ => Err(
            "compliance report requires either --policy-version-id with \
                  --input-set-id (preview) or --job-id (post-run read)"
                .to_owned(),
        ),
    }
}

pub async fn report(
    database_url: &str,
    local: Local,
    policy_version_id: Option<u64>,
    input_set_id: Option<u64>,
    job_id: Option<u64>,
) -> io::Result<i32> {
    let mode = match parse_report_mode(policy_version_id, input_set_id, job_id) {
        Ok(mode) => mode,
        Err(message) => {
            emit_err(
                "compliance",
                ErrorCode::BadArgs.as_str(),
                message,
                None,
                Some(local),
            )?;
            return Ok(1);
        }
    };
    match mode {
        ReportMode::Preview {
            policy_version_id,
            input_set_id,
        } => report_preview(database_url, local, policy_version_id, input_set_id).await,
        ReportMode::Run { job_id } => report_run(database_url, local, job_id).await,
    }
}

async fn report_preview(
    database_url: &str,
    local: Local,
    policy_version_id: u64,
    input_set_id: u64,
) -> io::Result<i32> {
    let cp = match open_control_plane("compliance", database_url, &local).await? {
        Ok(cp) => cp,
        Err(code) => return Ok(code),
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

async fn report_run(database_url: &str, local: Local, job_id: u64) -> io::Result<i32> {
    let cp = match open_control_plane("compliance", database_url, &local).await? {
        Ok(cp) => cp,
        Err(code) => return Ok(code),
    };
    match cp.read_compliance_run_report(JobId(job_id)).await {
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
    let cp = match open_control_plane("compliance", database_url, &local).await? {
        Ok(cp) => cp,
        Err(code) => return Ok(code),
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

/// Arguments for `compliance execute`, grouped to keep the handler within the
/// positional-parameter limit.
#[derive(Debug)]
pub struct ExecuteArgs {
    pub policy_version_id: u64,
    pub input_set_id: u64,
    pub staging_root: Option<std::path::PathBuf>,
    pub output_dir: Option<std::path::PathBuf>,
    pub safety_policy: Option<String>,
    pub backup_root: Option<std::path::PathBuf>,
}

pub async fn execute(database_url: &str, local: Local, args: ExecuteArgs) -> io::Result<i32> {
    let cp = match open_control_plane("compliance", database_url, &local).await? {
        Ok(cp) => cp,
        Err(code) => return Ok(code),
    };
    let mut options = ComplianceExecutionOptions {
        safety_policy_slug: args.safety_policy,
        backup_root: args.backup_root,
        ..ComplianceExecutionOptions::default()
    };
    if let Some(staging_root) = args.staging_root {
        options.apply_staging_root(staging_root);
    }
    if let Some(output_dir) = args.output_dir {
        options.apply_output_dir(output_dir);
    }
    match cp
        .execute_compliance_policy_with_options(
            PolicyVersionId(args.policy_version_id),
            PolicyInputSetId(args.input_set_id),
            options,
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
