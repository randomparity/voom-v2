//! `voom external-system register|list|show|health-check|sync|sync-report`.

use std::io;

use serde::Serialize;
use serde_json::Value as JsonValue;
use voom_control_plane::external::ExternalSyncReport;
use voom_core::{ExternalSystemId, format_iso8601};
use voom_store::repo::external::systems::{ExternalSystem, NewExternalSystem};

use crate::cli::{ExternalSystemCommand, ExternalSystemFields};
use crate::commands::common::{emit_voom_error, open_control_plane};
use crate::envelope::{Local, emit_err, emit_ok};

use super::path_mapping;

pub(crate) const COMMAND: &str = "external-system";

#[derive(Debug, Serialize)]
pub struct ExternalSystemWire {
    pub id: u64,
    pub kind: String,
    pub display_name: String,
    pub connection_profile: JsonValue,
    pub auth_ref: String,
    pub health_status: String,
    pub rate_limit_config: JsonValue,
    pub created_at: String,
    pub retired_at: Option<String>,
    pub epoch: u64,
}

impl From<ExternalSystem> for ExternalSystemWire {
    fn from(s: ExternalSystem) -> Self {
        Self {
            id: s.id.0,
            kind: s.kind.as_str().to_owned(),
            display_name: s.display_name,
            connection_profile: s.connection_profile,
            auth_ref: s.auth_ref,
            health_status: s.health_status.as_str().to_owned(),
            rate_limit_config: s.rate_limit_config,
            created_at: format_iso8601(s.created_at),
            retired_at: s.retired_at.map(format_iso8601),
            epoch: s.epoch,
        }
    }
}

#[derive(Debug, Serialize)]
struct ListData {
    external_systems: Vec<ExternalSystemWire>,
}

#[derive(Debug, Serialize)]
struct SyncReportWire {
    external_system_id: u64,
    health_status: String,
    active_link_count: u64,
    last_outcome: Option<String>,
    last_links_recorded: Option<u32>,
    last_links_retired: Option<u32>,
    last_started_at: Option<String>,
    last_finished_at: Option<String>,
}

impl From<ExternalSyncReport> for SyncReportWire {
    fn from(r: ExternalSyncReport) -> Self {
        Self {
            external_system_id: r.external_system_id,
            health_status: r.health_status,
            active_link_count: r.active_link_count,
            last_outcome: r.last_outcome,
            last_links_recorded: r.last_links_recorded,
            last_links_retired: r.last_links_retired,
            last_started_at: r.last_started_at.map(format_iso8601),
            last_finished_at: r.last_finished_at.map(format_iso8601),
        }
    }
}

pub async fn run(
    database_url: &str,
    local: Local,
    command: ExternalSystemCommand,
) -> io::Result<i32> {
    let cp = match open_control_plane(COMMAND, database_url, &local).await? {
        Ok(cp) => cp,
        Err(code) => return Ok(code),
    };
    match command {
        ExternalSystemCommand::Register(fields) => match build_input(fields) {
            Ok(input) => match cp.register_external_system(input).await {
                Ok(system) => emit_system(system, local),
                Err(err) => emit_voom_error(COMMAND, &err, local),
            },
            Err(message) => bad_args(message, local),
        },
        ExternalSystemCommand::List => match cp.list_external_systems().await {
            Ok(systems) => emit_ok(
                COMMAND,
                ListData {
                    external_systems: systems.into_iter().map(ExternalSystemWire::from).collect(),
                },
                Some(local),
                Vec::new(),
            )
            .map(|()| 0),
            Err(err) => emit_voom_error(COMMAND, &err, local),
        },
        ExternalSystemCommand::Show { id } => {
            match cp.get_external_system(ExternalSystemId(id)).await {
                Ok(Some(system)) => emit_system(system, local),
                Ok(None) => not_found(id, local),
                Err(err) => emit_voom_error(COMMAND, &err, local),
            }
        }
        ExternalSystemCommand::HealthCheck { id } => {
            match cp.health_check_external_system(ExternalSystemId(id)).await {
                Ok(system) => emit_system(system, local),
                Err(err) => emit_voom_error(COMMAND, &err, local),
            }
        }
        ExternalSystemCommand::Sync { id } => {
            match cp.sync_external_system(ExternalSystemId(id)).await {
                Ok(report) => emit_report(report, local),
                Err(err) => emit_voom_error(COMMAND, &err, local),
            }
        }
        ExternalSystemCommand::SyncReport { id } => {
            match cp.external_sync_report(ExternalSystemId(id)).await {
                Ok(report) => emit_report(report, local),
                Err(err) => emit_voom_error(COMMAND, &err, local),
            }
        }
        ExternalSystemCommand::PathMapping(command) => path_mapping::run(&cp, local, command).await,
    }
}

/// Translate CLI fields into the store input, rejecting invalid JSON documents
/// with a `BAD_ARGS` message.
fn build_input(fields: ExternalSystemFields) -> Result<NewExternalSystem, String> {
    let connection_profile = parse_json(&fields.connection_profile, "connection-profile")?;
    let rate_limit_config = parse_json(&fields.rate_limit_config, "rate-limit-config")?;
    Ok(NewExternalSystem {
        kind: fields.kind.to_store(),
        display_name: fields.display_name,
        connection_profile,
        auth_ref: fields.auth_ref,
        rate_limit_config,
    })
}

fn parse_json(raw: &str, field: &str) -> Result<JsonValue, String> {
    serde_json::from_str(raw).map_err(|e| format!("--{field} is not valid JSON: {e}"))
}

fn emit_system(system: ExternalSystem, local: Local) -> io::Result<i32> {
    emit_ok(
        COMMAND,
        ExternalSystemWire::from(system),
        Some(local),
        Vec::new(),
    )
    .map(|()| 0)
}

fn emit_report(report: ExternalSyncReport, local: Local) -> io::Result<i32> {
    emit_ok(
        COMMAND,
        SyncReportWire::from(report),
        Some(local),
        Vec::new(),
    )
    .map(|()| 0)
}

fn not_found(id: u64, local: Local) -> io::Result<i32> {
    emit_err(
        COMMAND,
        voom_core::ErrorCode::NotFound.as_str(),
        format!("external system id={id} not found"),
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
