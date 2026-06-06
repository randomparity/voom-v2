use std::fmt::Write as _;
use std::io;
use std::path::Path;

use serde::Serialize;
use voom_control_plane::ControlPlane;
use voom_control_plane::policy::{
    PolicyInputFromScanInput, PolicyInputFromScanResult, PolicyMutationError,
};
use voom_core::{FileVersionId, MediaSnapshotId, PolicyDocumentId};
use voom_store::repo::policies::{
    CreatedPolicyVersion, PolicyDocument, PolicyDocumentSummary, PolicyVersion,
};

use crate::cli::{PolicyCommand, PolicyInputCommand, PolicyVersionCommand};
use crate::commands::common::{emit_voom_error, open_control_plane};
use crate::envelope::{Local, emit_err, emit_ok};

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

#[derive(Debug, Serialize)]
pub struct PolicyDocumentWire {
    pub document_id: u64,
    pub slug: String,
    pub display_name: String,
    pub current_accepted_version_id: Option<u64>,
    pub epoch: u64,
}

#[derive(Debug, Serialize)]
pub struct PolicyVersionWire {
    pub version_id: u64,
    pub document_id: u64,
    pub version_number: u64,
    pub source_hash: String,
    pub schema_version: u32,
}

#[derive(Debug, Serialize)]
pub struct PolicyCreateData {
    pub document: PolicyDocumentWire,
    pub version: PolicyVersionWire,
}

#[derive(Debug, Serialize)]
pub struct PolicyVersionAddData {
    pub version: PolicyVersionWire,
}

#[derive(Debug, Serialize)]
pub struct PolicyListData {
    pub documents: Vec<PolicyDocumentWire>,
}

#[derive(Debug, Serialize)]
pub struct PolicyShowData {
    pub document: PolicyDocumentWire,
    pub versions: Vec<PolicyVersionWire>,
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
        PolicyCommand::Create { slug, file } => create_document(&cp, local, &slug, &file).await,
        PolicyCommand::Version(PolicyVersionCommand::Add { document_id, file }) => {
            add_version(&cp, local, PolicyDocumentId(document_id), &file).await
        }
        PolicyCommand::List => list_documents(&cp, local).await,
        PolicyCommand::Show { document_id } => {
            show_document(&cp, local, PolicyDocumentId(document_id)).await
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

async fn create_document(
    cp: &ControlPlane,
    local: Local,
    slug: &str,
    file: &Path,
) -> io::Result<i32> {
    let source = match read_source(file, &local)? {
        Ok(source) => source,
        Err(code) => return Ok(code),
    };
    match cp.create_policy_document(slug, &source).await {
        Ok(created) => emit_ok(
            "policy",
            PolicyCreateData::from(created),
            Some(local),
            Vec::new(),
        )
        .map(|()| 0),
        Err(err) => emit_mutation_error(&err, local),
    }
}

async fn add_version(
    cp: &ControlPlane,
    local: Local,
    document_id: PolicyDocumentId,
    file: &Path,
) -> io::Result<i32> {
    let source = match read_source(file, &local)? {
        Ok(source) => source,
        Err(code) => return Ok(code),
    };
    match cp.add_policy_version(document_id, &source).await {
        Ok(version) => emit_ok(
            "policy",
            PolicyVersionAddData {
                version: PolicyVersionWire::from(version),
            },
            Some(local),
            Vec::new(),
        )
        .map(|()| 0),
        Err(err) => emit_mutation_error(&err, local),
    }
}

async fn list_documents(cp: &ControlPlane, local: Local) -> io::Result<i32> {
    match cp.list_policy_documents().await {
        Ok(documents) => emit_ok(
            "policy",
            PolicyListData {
                documents: documents
                    .into_iter()
                    .map(PolicyDocumentWire::from)
                    .collect(),
            },
            Some(local),
            Vec::new(),
        )
        .map(|()| 0),
        Err(err) => emit_voom_error("policy", &err, local),
    }
}

async fn show_document(
    cp: &ControlPlane,
    local: Local,
    document_id: PolicyDocumentId,
) -> io::Result<i32> {
    let document = match cp.get_policy_document(document_id).await {
        Ok(Some(document)) => document,
        Ok(None) => {
            emit_err(
                "policy",
                voom_core::ErrorCode::NotFound.as_str(),
                format!("policy document {document_id} not found"),
                None,
                Some(local),
            )?;
            return Ok(2);
        }
        Err(err) => return emit_voom_error("policy", &err, local),
    };
    let versions = match cp.list_policy_versions(document_id).await {
        Ok(versions) => versions,
        Err(err) => return emit_voom_error("policy", &err, local),
    };
    emit_ok(
        "policy",
        PolicyShowData {
            document: PolicyDocumentWire::from(document),
            versions: versions.into_iter().map(PolicyVersionWire::from).collect(),
        },
        Some(local),
        Vec::new(),
    )
    .map(|()| 0)
}

/// Read a `.voom` source file, emitting a `BAD_ARGS` envelope on failure.
///
/// Returns `Ok(Ok(source))` on success and `Ok(Err(1))` when the read failed
/// and an error envelope has already been emitted.
fn read_source(file: &Path, local: &Local) -> io::Result<Result<String, i32>> {
    match std::fs::read_to_string(file) {
        Ok(source) => Ok(Ok(source)),
        Err(err) => {
            emit_err(
                "policy",
                voom_core::ErrorCode::BadArgs.as_str(),
                format!("could not read policy file {}: {err}", file.display()),
                Some("Pass --file pointing to a readable .voom file".to_owned()),
                Some(local.clone()),
            )?;
            Ok(Err(1))
        }
    }
}

fn emit_mutation_error(err: &PolicyMutationError, local: Local) -> io::Result<i32> {
    emit_err(
        "policy",
        err.code(),
        mutation_error_message(err),
        None,
        Some(local),
    )?;
    Ok(2)
}

fn mutation_error_message(err: &PolicyMutationError) -> String {
    match err {
        PolicyMutationError::Compile(compile) => {
            let mut message = compile.error.to_string();
            for diagnostic in &compile.diagnostics {
                let _ = write!(message, " [{}] {}", diagnostic.code, diagnostic.message);
            }
            message
        }
        PolicyMutationError::Store(err) => err.to_string(),
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

impl From<CreatedPolicyVersion> for PolicyCreateData {
    fn from(created: CreatedPolicyVersion) -> Self {
        Self {
            document: PolicyDocumentWire::from(created.document),
            version: PolicyVersionWire::from(created.version),
        }
    }
}

impl From<PolicyDocument> for PolicyDocumentWire {
    fn from(document: PolicyDocument) -> Self {
        Self {
            document_id: document.id.0,
            slug: document.slug,
            display_name: document.display_name,
            current_accepted_version_id: document.current_accepted_version_id.map(|id| id.0),
            epoch: document.epoch,
        }
    }
}

impl From<PolicyDocumentSummary> for PolicyDocumentWire {
    fn from(summary: PolicyDocumentSummary) -> Self {
        Self {
            document_id: summary.id.0,
            slug: summary.slug,
            display_name: summary.display_name,
            current_accepted_version_id: summary.current_accepted_version_id.map(|id| id.0),
            epoch: summary.epoch,
        }
    }
}

impl From<PolicyVersion> for PolicyVersionWire {
    fn from(version: PolicyVersion) -> Self {
        Self {
            version_id: version.id.0,
            document_id: version.policy_document_id.0,
            version_number: version.version_number,
            source_hash: version.source_hash,
            schema_version: version.schema_version,
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
