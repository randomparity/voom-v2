use std::io;

use serde::Serialize;
use voom_control_plane::ControlPlane;
use voom_core::OperationKind;
use voom_store::repo::safety_policies::{NewSafetyPolicy, SafetyPolicy};

use crate::cli::{SafetyPolicyCommand, SafetyPolicyFields};
use crate::commands::common::{emit_voom_error, open_control_plane};
use crate::envelope::{Local, emit_err, emit_ok};

const COMMAND: &str = "safety-policy";

#[expect(
    clippy::struct_excessive_bools,
    reason = "wire projection of the safety policy's four independent spec-mandated toggles"
)]
#[derive(Debug, Serialize)]
pub struct SafetyPolicyWire {
    pub id: u64,
    pub slug: String,
    pub display_name: String,
    pub schema_version: u32,
    pub auto_execute_operations: Vec<String>,
    pub backup_required: bool,
    pub approval_required: bool,
    pub allowed_commit_modes: Vec<String>,
    pub verification_level: String,
    pub block_on_failed_records: bool,
    pub block_on_recovery_required_records: bool,
    pub created_at: String,
    pub updated_at: String,
}

impl From<SafetyPolicy> for SafetyPolicyWire {
    fn from(policy: SafetyPolicy) -> Self {
        Self {
            id: policy.id,
            slug: policy.slug,
            display_name: policy.display_name,
            schema_version: policy.schema_version,
            auto_execute_operations: policy
                .auto_execute_operations
                .iter()
                .map(|o| o.as_str().to_owned())
                .collect(),
            backup_required: policy.backup_required,
            approval_required: policy.approval_required,
            allowed_commit_modes: policy
                .allowed_commit_modes
                .iter()
                .map(|m| m.as_str().to_owned())
                .collect(),
            verification_level: policy.verification_level.as_str().to_owned(),
            block_on_failed_records: policy.block_on_failed_records,
            block_on_recovery_required_records: policy.block_on_recovery_required_records,
            created_at: voom_core::format_iso8601(policy.created_at),
            updated_at: voom_core::format_iso8601(policy.updated_at),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct SafetyPolicyListData {
    pub policies: Vec<SafetyPolicyWire>,
}

#[derive(Debug, Serialize)]
pub struct DeleteData {
    pub slug: String,
    pub deleted: bool,
}

pub async fn run(
    database_url: &str,
    local: Local,
    command: SafetyPolicyCommand,
) -> io::Result<i32> {
    let cp = match open_control_plane(COMMAND, database_url, &local).await? {
        Ok(cp) => cp,
        Err(code) => return Ok(code),
    };
    match command {
        SafetyPolicyCommand::Create(fields) => match build_input(fields) {
            Ok(input) => emit_one(cp.create_safety_policy(input).await, local),
            Err(message) => bad_args(message, local),
        },
        SafetyPolicyCommand::List => list(&cp, local).await,
        SafetyPolicyCommand::Show { slug } => {
            emit_optional(cp.get_safety_policy(&slug).await, &slug, local)
        }
        SafetyPolicyCommand::Update(fields) => {
            let slug = fields.slug.clone();
            match build_input(fields) {
                Ok(input) => emit_optional(cp.update_safety_policy(input).await, &slug, local),
                Err(message) => bad_args(message, local),
            }
        }
        SafetyPolicyCommand::Delete { slug } => delete(&cp, &slug, local).await,
    }
}

/// Translate CLI fields into the store input, rejecting an unknown operation
/// token with a `BAD_ARGS` message.
fn build_input(fields: SafetyPolicyFields) -> Result<NewSafetyPolicy, String> {
    let mut operations = Vec::with_capacity(fields.auto_execute_operations.len());
    for token in &fields.auto_execute_operations {
        let operation = OperationKind::from_wire(token)
            .ok_or_else(|| format!("unknown operation kind {token:?}"))?;
        operations.push(operation);
    }
    Ok(NewSafetyPolicy {
        slug: fields.slug,
        display_name: fields.display_name,
        auto_execute_operations: operations,
        backup_required: fields.backup_required,
        approval_required: fields.approval_required,
        allowed_commit_modes: fields
            .allowed_commit_modes
            .iter()
            .map(|m| m.to_store())
            .collect(),
        verification_level: fields.verification_level.to_store(),
        block_on_failed_records: fields.block_on_failed_records,
        block_on_recovery_required_records: fields.block_on_recovery_required_records,
    })
}

async fn list(cp: &ControlPlane, local: Local) -> io::Result<i32> {
    match cp.list_safety_policies().await {
        Ok(policies) => emit_ok(
            COMMAND,
            SafetyPolicyListData {
                policies: policies.into_iter().map(SafetyPolicyWire::from).collect(),
            },
            Some(local),
            Vec::new(),
        )
        .map(|()| 0),
        Err(err) => emit_voom_error(COMMAND, &err, local),
    }
}

async fn delete(cp: &ControlPlane, slug: &str, local: Local) -> io::Result<i32> {
    match cp.delete_safety_policy(slug).await {
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

fn emit_one(result: Result<SafetyPolicy, voom_core::VoomError>, local: Local) -> io::Result<i32> {
    match result {
        Ok(policy) => emit_ok(
            COMMAND,
            SafetyPolicyWire::from(policy),
            Some(local),
            Vec::new(),
        )
        .map(|()| 0),
        Err(err) => emit_voom_error(COMMAND, &err, local),
    }
}

fn emit_optional(
    result: Result<Option<SafetyPolicy>, voom_core::VoomError>,
    slug: &str,
    local: Local,
) -> io::Result<i32> {
    match result {
        Ok(Some(policy)) => emit_ok(
            COMMAND,
            SafetyPolicyWire::from(policy),
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
        format!("safety policy {slug:?} not found"),
        None,
        Some(local),
    )?;
    Ok(2)
}

fn bad_args(message: String, local: Local) -> io::Result<i32> {
    emit_err(
        COMMAND,
        voom_core::ErrorCode::BadArgs.as_str(),
        message,
        None,
        Some(local),
    )?;
    Ok(1)
}
