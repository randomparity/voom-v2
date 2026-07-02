use std::io;

use serde::Serialize;
use voom_control_plane::ControlPlane;
use voom_store::repo::scheduling_policies::{NewSchedulingPolicy, SchedulingPolicy};

use crate::cli::SchedulingPolicyCommand;
use crate::commands::common::{emit_voom_error, open_control_plane};
use crate::envelope::{Local, emit_err, emit_ok};

const COMMAND: &str = "scheduling-policy";

#[derive(Debug, Serialize)]
pub struct SchedulingPolicyWire {
    pub id: u64,
    pub slug: String,
    pub display_name: String,
    pub schema_version: u32,
    pub priority: String,
    pub copy_window: Option<String>,
    pub large_jobs_night_only: bool,
    pub pause_on_degraded_node: bool,
    pub created_at: String,
    pub updated_at: String,
}

impl From<SchedulingPolicy> for SchedulingPolicyWire {
    fn from(policy: SchedulingPolicy) -> Self {
        Self {
            id: policy.id,
            slug: policy.slug,
            display_name: policy.display_name,
            schema_version: policy.schema_version,
            priority: policy.priority.as_str().to_owned(),
            copy_window: policy.copy_window,
            large_jobs_night_only: policy.large_jobs_night_only,
            pause_on_degraded_node: policy.pause_on_degraded_node,
            created_at: voom_core::format_iso8601(policy.created_at),
            updated_at: voom_core::format_iso8601(policy.updated_at),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct SchedulingPolicyListData {
    pub policies: Vec<SchedulingPolicyWire>,
}

#[derive(Debug, Serialize)]
pub struct DeleteData {
    pub slug: String,
    pub deleted: bool,
}

pub async fn run(
    database_url: &str,
    local: Local,
    command: SchedulingPolicyCommand,
) -> io::Result<i32> {
    let cp = match open_control_plane(COMMAND, database_url, &local).await? {
        Ok(cp) => cp,
        Err(code) => return Ok(code),
    };
    match command {
        SchedulingPolicyCommand::Create {
            slug,
            display_name,
            priority,
            copy_window,
            large_jobs_night_only,
            pause_on_degraded_node,
        } => {
            let input = NewSchedulingPolicy {
                slug,
                display_name,
                priority: priority.to_store(),
                copy_window,
                large_jobs_night_only,
                pause_on_degraded_node,
            };
            emit_one(cp.create_scheduling_policy(input).await, local)
        }
        SchedulingPolicyCommand::List => list(&cp, local).await,
        SchedulingPolicyCommand::Show { slug } => {
            emit_optional(cp.get_scheduling_policy(&slug).await, &slug, local)
        }
        SchedulingPolicyCommand::Update {
            slug,
            display_name,
            priority,
            copy_window,
            large_jobs_night_only,
            pause_on_degraded_node,
        } => {
            let input = NewSchedulingPolicy {
                slug: slug.clone(),
                display_name,
                priority: priority.to_store(),
                copy_window,
                large_jobs_night_only,
                pause_on_degraded_node,
            };
            emit_optional(cp.update_scheduling_policy(input).await, &slug, local)
        }
        SchedulingPolicyCommand::Delete { slug } => delete(&cp, &slug, local).await,
    }
}

async fn list(cp: &ControlPlane, local: Local) -> io::Result<i32> {
    match cp.list_scheduling_policies().await {
        Ok(policies) => emit_ok(
            COMMAND,
            SchedulingPolicyListData {
                policies: policies
                    .into_iter()
                    .map(SchedulingPolicyWire::from)
                    .collect(),
            },
            Some(local),
            Vec::new(),
        )
        .map(|()| 0),
        Err(err) => emit_voom_error(COMMAND, &err, local),
    }
}

async fn delete(cp: &ControlPlane, slug: &str, local: Local) -> io::Result<i32> {
    match cp.delete_scheduling_policy(slug).await {
        Ok(true) => emit_ok(
            COMMAND,
            DeleteData {
                slug: slug.to_owned(),
                deleted: true,
            },
            Some(local),
            Vec::new(),
        )
        .map(|()| 0),
        Ok(false) => not_found(slug, local),
        Err(err) => emit_voom_error(COMMAND, &err, local),
    }
}

fn emit_one(
    result: Result<SchedulingPolicy, voom_core::VoomError>,
    local: Local,
) -> io::Result<i32> {
    match result {
        Ok(policy) => emit_ok(
            COMMAND,
            SchedulingPolicyWire::from(policy),
            Some(local),
            Vec::new(),
        )
        .map(|()| 0),
        Err(err) => emit_voom_error(COMMAND, &err, local),
    }
}

fn emit_optional(
    result: Result<Option<SchedulingPolicy>, voom_core::VoomError>,
    slug: &str,
    local: Local,
) -> io::Result<i32> {
    match result {
        Ok(Some(policy)) => emit_ok(
            COMMAND,
            SchedulingPolicyWire::from(policy),
            Some(local),
            Vec::new(),
        )
        .map(|()| 0),
        Ok(None) => not_found(slug, local),
        Err(err) => emit_voom_error(COMMAND, &err, local),
    }
}

fn not_found(slug: &str, local: Local) -> io::Result<i32> {
    emit_err(
        COMMAND,
        voom_core::ErrorCode::NotFound.as_str(),
        format!("scheduling policy {slug:?} not found"),
        None,
        Some(local),
    )?;
    Ok(2)
}
