//! `voom external-system path-mapping create|list|show|update|delete`.

use std::io;

use serde::Serialize;
use voom_control_plane::ControlPlane;
use voom_core::{ExternalPathMappingId, ExternalSystemId, format_iso8601};
use voom_store::repo::external::path_mappings::{
    ExternalPathMapping, NewExternalPathMapping, PathMappingUpdate,
};

use crate::cli::{
    ExternalPathMappingCommand, ExternalPathMappingFields, ExternalPathMappingUpdateFields,
};
use crate::commands::common::emit_voom_error;
use crate::envelope::{Local, emit_err, emit_ok};

pub(crate) const COMMAND: &str = "external-system path-mapping";

#[derive(Debug, Serialize)]
pub struct PathMappingWire {
    pub id: u64,
    pub external_system_id: u64,
    pub internal_prefix: String,
    pub external_prefix: String,
    pub visibility: String,
    pub created_at: String,
    pub retired_at: Option<String>,
}

impl From<ExternalPathMapping> for PathMappingWire {
    fn from(m: ExternalPathMapping) -> Self {
        Self {
            id: m.id.0,
            external_system_id: m.external_system_id.0,
            internal_prefix: m.internal_prefix,
            external_prefix: m.external_prefix,
            visibility: m.visibility.as_str().to_owned(),
            created_at: format_iso8601(m.created_at),
            retired_at: m.retired_at.map(format_iso8601),
        }
    }
}

#[derive(Debug, Serialize)]
struct ListData {
    path_mappings: Vec<PathMappingWire>,
}

#[derive(Debug, Serialize)]
struct DeleteData {
    id: u64,
    deleted: bool,
}

pub async fn run(
    cp: &ControlPlane,
    local: Local,
    command: ExternalPathMappingCommand,
) -> io::Result<i32> {
    match command {
        ExternalPathMappingCommand::Create(fields) => create(cp, fields, local).await,
        ExternalPathMappingCommand::List { system_id } => list(cp, system_id, local).await,
        ExternalPathMappingCommand::Show { id } => show(cp, id, local).await,
        ExternalPathMappingCommand::Update(fields) => update(cp, fields, local).await,
        ExternalPathMappingCommand::Delete { id } => delete(cp, id, local).await,
    }
}

async fn create(
    cp: &ControlPlane,
    fields: ExternalPathMappingFields,
    local: Local,
) -> io::Result<i32> {
    let input = NewExternalPathMapping {
        external_system_id: ExternalSystemId(fields.system_id),
        internal_prefix: fields.internal_prefix,
        external_prefix: fields.external_prefix,
        visibility: fields.visibility.to_store(),
    };
    match cp.create_external_path_mapping(input).await {
        Ok(mapping) => emit_one(mapping, local),
        Err(err) => emit_voom_error(COMMAND, &err, local),
    }
}

async fn list(cp: &ControlPlane, system_id: u64, local: Local) -> io::Result<i32> {
    match cp
        .list_external_path_mappings(ExternalSystemId(system_id))
        .await
    {
        Ok(mappings) => emit_ok(
            COMMAND,
            ListData {
                path_mappings: mappings.into_iter().map(PathMappingWire::from).collect(),
            },
            Some(local),
            Vec::new(),
        )
        .map(|()| 0),
        Err(err) => emit_voom_error(COMMAND, &err, local),
    }
}

async fn show(cp: &ControlPlane, id: u64, local: Local) -> io::Result<i32> {
    match cp
        .get_external_path_mapping(ExternalPathMappingId(id))
        .await
    {
        Ok(Some(mapping)) => emit_one(mapping, local),
        Ok(None) => not_found(id, local),
        Err(err) => emit_voom_error(COMMAND, &err, local),
    }
}

async fn update(
    cp: &ControlPlane,
    fields: ExternalPathMappingUpdateFields,
    local: Local,
) -> io::Result<i32> {
    let update = PathMappingUpdate {
        internal_prefix: fields.internal_prefix,
        external_prefix: fields.external_prefix,
        visibility: fields
            .visibility
            .map(crate::cli::PathVisibilityArg::to_store),
    };
    match cp
        .update_external_path_mapping(ExternalPathMappingId(fields.id), update)
        .await
    {
        Ok(Some(mapping)) => emit_one(mapping, local),
        Ok(None) => not_found(fields.id, local),
        Err(err) => emit_voom_error(COMMAND, &err, local),
    }
}

async fn delete(cp: &ControlPlane, id: u64, local: Local) -> io::Result<i32> {
    match cp
        .delete_external_path_mapping(ExternalPathMappingId(id))
        .await
    {
        Ok(true) => emit_ok(
            COMMAND,
            DeleteData { id, deleted: true },
            Some(local),
            Vec::new(),
        )
        .map(|()| 0),
        Ok(false) => not_found(id, local),
        Err(err) => emit_voom_error(COMMAND, &err, local),
    }
}

fn emit_one(mapping: ExternalPathMapping, local: Local) -> io::Result<i32> {
    emit_ok(
        COMMAND,
        PathMappingWire::from(mapping),
        Some(local),
        Vec::new(),
    )
    .map(|()| 0)
}

fn not_found(id: u64, local: Local) -> io::Result<i32> {
    emit_err(
        COMMAND,
        voom_core::ErrorCode::NotFound.as_str(),
        format!("external path mapping id={id} not found"),
        None,
        Some(local),
    )?;
    Ok(2)
}
