use std::io;

use serde::Serialize;
use voom_control_plane::ControlPlane;
use voom_control_plane::cases::policy::policy_inputs::{
    PolicyInputFromScanInput, PolicyInputFromScanResult,
};
use voom_core::{FileVersionId, MediaSnapshotId};

use crate::cli::{PolicyCommand, PolicyInputCommand};
use crate::commands::common::{emit_voom_error, open_control_plane};
use crate::envelope::{Local, emit_ok};

#[derive(Debug, Serialize)]
pub struct PolicyInputCreateFromScanData {
    pub input_set: PolicyInputCreateFromScanSummary,
}

#[derive(Debug, Serialize)]
pub struct PolicyInputCreateFromScanSummary {
    pub input_set_id: u64,
    pub slug: String,
    pub source_kind: String,
    pub file_version_id: u64,
    pub media_snapshot_id: u64,
}

pub async fn run(database_url: &str, local: Local, command: PolicyCommand) -> io::Result<i32> {
    let cp = match open_control_plane("policy", database_url, &local).await? {
        Ok(cp) => cp,
        Err(code) => return Ok(code),
    };
    match command {
        PolicyCommand::Input(PolicyInputCommand::CreateFromScan {
            slug,
            file_version_id,
            media_snapshot_id,
            container,
            video_codec,
        }) => {
            create_from_scan(
                &cp,
                local,
                PolicyInputFromScanInput {
                    slug,
                    file_version_id: FileVersionId(file_version_id),
                    media_snapshot_id: MediaSnapshotId(media_snapshot_id),
                    container,
                    video_codec,
                },
            )
            .await
        }
    }
}

async fn create_from_scan(
    cp: &ControlPlane,
    local: Local,
    input: PolicyInputFromScanInput,
) -> io::Result<i32> {
    match cp.create_policy_input_set_from_scan(input).await {
        Ok(result) => emit_ok(
            "policy",
            PolicyInputCreateFromScanData::from(result),
            Some(local),
            Vec::new(),
        )
        .map(|()| 0),
        Err(err) => emit_voom_error("policy", &err, local),
    }
}

impl From<PolicyInputFromScanResult> for PolicyInputCreateFromScanData {
    fn from(result: PolicyInputFromScanResult) -> Self {
        Self {
            input_set: PolicyInputCreateFromScanSummary {
                input_set_id: result.input_set_id.0,
                slug: result.slug,
                source_kind: source_kind_wire(result.source_kind).to_owned(),
                file_version_id: result.file_version_id.0,
                media_snapshot_id: result.media_snapshot_id.0,
            },
        }
    }
}

#[must_use]
pub fn source_kind_wire(kind: voom_policy::PolicyInputSourceKind) -> &'static str {
    kind.as_str()
}

#[cfg(test)]
#[path = "policy_test.rs"]
mod tests;
