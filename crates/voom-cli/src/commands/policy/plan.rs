use std::{io, path::Path};

use serde::Serialize;
use voom_core::{ErrorCode, PolicyInputSetId, PolicyVersionId};
use voom_policy::{FixtureName, load_fixture};

use crate::commands::common::open_control_plane;
use crate::envelope::{Local, emit_err, emit_ok};

#[derive(Debug, Serialize)]
pub struct PlanData {
    pub plan: voom_plan::ExecutionPlan,
}

pub async fn dry_run(policy_file: &Path, input_fixture: &str) -> io::Result<i32> {
    let fixture = match fixture_name(input_fixture) {
        Ok(fixture) => fixture,
        Err(message) => {
            emit_err("plan", ErrorCode::BadArgs.as_str(), message, None, None)?;
            return Ok(1);
        }
    };
    let input = match load_fixture(fixture) {
        Ok(input) => input,
        Err(err) => {
            emit_err(
                "plan",
                ErrorCode::Internal.as_str(),
                err.to_string(),
                None,
                None,
            )?;
            return Ok(2);
        }
    };
    let source = match tokio::fs::read_to_string(policy_file).await {
        Ok(source) => source,
        Err(err) => {
            emit_err(
                "plan",
                ErrorCode::BadArgs.as_str(),
                err.to_string(),
                None,
                None,
            )?;
            return Ok(1);
        }
    };

    match voom_control_plane::plan_policy_source_with_input(&source, input, Some(input_fixture)) {
        Ok(plan) => emit_ok("plan", PlanData { plan }, None, Vec::new()).map(|()| 0),
        Err(err) => {
            emit_err("plan", err.code(), err.to_string(), None, None)?;
            Ok(2)
        }
    }
}

pub async fn show(
    database_url: &str,
    local: Local,
    policy_version_id: u64,
    input_set_id: u64,
) -> io::Result<i32> {
    let cp = match open_control_plane("plan", database_url, &local).await? {
        Ok(cp) => cp,
        Err(code) => return Ok(code),
    };

    match cp
        .plan_accepted_policy_version_with_input_set(
            PolicyVersionId(policy_version_id),
            PolicyInputSetId(input_set_id),
        )
        .await
    {
        Ok(plan) => emit_ok("plan", PlanData { plan }, Some(local), Vec::new()).map(|()| 0),
        Err(err) => {
            emit_err("plan", err.code(), err.to_string(), None, Some(local))?;
            Ok(2)
        }
    }
}

pub fn fixture_name(label: &str) -> Result<FixtureName, String> {
    label
        .parse()
        .map_err(|voom_policy::fixtures::UnknownFixtureName| "unknown input fixture".to_owned())
}

#[cfg(test)]
#[path = "plan_test.rs"]
mod tests;
