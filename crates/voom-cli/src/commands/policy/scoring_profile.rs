use std::io;

use serde::Serialize;
use serde_json::Value;
use voom_control_plane::ControlPlane;
use voom_store::repo::quality_scoring_profiles::{NewQualityScoringProfile, QualityScoringProfile};

use crate::cli::ScoringProfileCommand;
use crate::commands::common::{emit_voom_error, open_control_plane};
use crate::envelope::{Local, emit_err, emit_ok};

const COMMAND: &str = "scoring-profile";

#[derive(Debug, Serialize)]
pub struct ScoringProfileWire {
    pub name: String,
    pub version: u32,
    pub definition: Value,
    pub created_at: String,
    pub retired_at: Option<String>,
}

impl From<QualityScoringProfile> for ScoringProfileWire {
    fn from(profile: QualityScoringProfile) -> Self {
        Self {
            name: profile.name,
            version: profile.version,
            definition: profile.definition,
            created_at: voom_core::format_iso8601(profile.created_at),
            retired_at: profile.retired_at.map(voom_core::format_iso8601),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ScoringProfileListData {
    pub profiles: Vec<ScoringProfileWire>,
}

pub async fn run(
    database_url: &str,
    local: Local,
    command: ScoringProfileCommand,
) -> io::Result<i32> {
    let cp = match open_control_plane(COMMAND, database_url, &local).await? {
        Ok(cp) => cp,
        Err(code) => return Ok(code),
    };
    match command {
        ScoringProfileCommand::Create {
            name,
            version,
            definition,
        } => match parse_definition(&definition, local.clone())? {
            Ok(definition) => emit_one(
                cp.create_scoring_profile(NewQualityScoringProfile {
                    name,
                    version,
                    definition,
                })
                .await,
                local,
            ),
            Err(code) => Ok(code),
        },
        ScoringProfileCommand::List => list(&cp, local).await,
        ScoringProfileCommand::Show { name } => {
            emit_optional(cp.get_scoring_profile(&name).await, &name, local)
        }
        ScoringProfileCommand::Update {
            name,
            version,
            definition,
        } => match parse_definition(&definition, local.clone())? {
            Ok(definition) => emit_optional(
                cp.update_scoring_profile(NewQualityScoringProfile {
                    name: name.clone(),
                    version,
                    definition,
                })
                .await,
                &name,
                local,
            ),
            Err(code) => Ok(code),
        },
        ScoringProfileCommand::Retire { name } => {
            emit_optional(cp.retire_scoring_profile(&name).await, &name, local)
        }
    }
}

/// Parse the `--definition` JSON string. A syntax error is a bad argument
/// (exit 1); object/scalar validation is left to the repository (`CONFIG_INVALID`).
fn parse_definition(raw: &str, local: Local) -> io::Result<Result<Value, i32>> {
    match serde_json::from_str::<Value>(raw) {
        Ok(value) => Ok(Ok(value)),
        Err(err) => {
            emit_err(
                COMMAND,
                "BAD_ARGS",
                format!("--definition is not valid JSON: {err}"),
                None,
                Some(local),
            )?;
            Ok(Err(1))
        }
    }
}

async fn list(cp: &ControlPlane, local: Local) -> io::Result<i32> {
    match cp.list_scoring_profiles().await {
        Ok(profiles) => emit_ok(
            COMMAND,
            ScoringProfileListData {
                profiles: profiles.into_iter().map(ScoringProfileWire::from).collect(),
            },
            Some(local),
            Vec::new(),
        )
        .map(|()| 0),
        Err(err) => emit_voom_error(COMMAND, &err, local),
    }
}

fn emit_one(
    result: Result<QualityScoringProfile, voom_core::VoomError>,
    local: Local,
) -> io::Result<i32> {
    match result {
        Ok(profile) => emit_ok(
            COMMAND,
            ScoringProfileWire::from(profile),
            Some(local),
            Vec::new(),
        )
        .map(|()| 0),
        Err(err) => emit_voom_error(COMMAND, &err, local),
    }
}

fn emit_optional(
    result: Result<Option<QualityScoringProfile>, voom_core::VoomError>,
    name: &str,
    local: Local,
) -> io::Result<i32> {
    match result {
        Ok(Some(profile)) => emit_ok(
            COMMAND,
            ScoringProfileWire::from(profile),
            Some(local),
            Vec::new(),
        )
        .map(|()| 0),
        Ok(None) => not_found(name, local),
        Err(err) => emit_voom_error(COMMAND, &err, local),
    }
}

fn not_found(name: &str, local: Local) -> io::Result<i32> {
    emit_err(
        COMMAND,
        voom_core::ErrorCode::NotFound.as_str(),
        format!("scoring profile {name:?} not found"),
        None,
        Some(local),
    )?;
    Ok(2)
}
