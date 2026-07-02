//! `voom library <add|list|show|update|enable|disable|remove>` and delegation
//! to the nested `voom library root ...` surface.

use std::io;

use serde::Serialize;
use voom_control_plane::ControlPlane;
use voom_core::{LibraryId, format_iso8601};
use voom_store::repo::library::libraries::{Library, LibraryUpdate, NewLibrary};

use crate::cli::{LibraryCommand, LibraryMediaKindArg};
use crate::commands::common::{emit_voom_error, open_control_plane};
use crate::envelope::{Local, emit_err, emit_ok};

use super::{COMMAND, root};

#[derive(Debug, Serialize)]
pub struct LibraryData {
    pub library_id: u64,
    pub slug: String,
    pub display_name: String,
    pub media_kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_scoring_profile_name: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl From<Library> for LibraryData {
    fn from(library: Library) -> Self {
        Self {
            library_id: library.id.0,
            slug: library.slug,
            display_name: library.display_name,
            media_kind: library.media_kind.as_str().to_owned(),
            description: library.description,
            enabled: library.enabled,
            default_scoring_profile_name: library.default_scoring_profile_name,
            created_at: format_iso8601(library.created_at),
            updated_at: format_iso8601(library.updated_at),
        }
    }
}

#[derive(Debug, Serialize)]
struct ListData {
    libraries: Vec<LibraryData>,
}

#[derive(Debug, Serialize)]
struct RemoveData {
    library_id: u64,
    removed: bool,
}

pub async fn run(database_url: &str, local: Local, command: LibraryCommand) -> io::Result<i32> {
    let cp = match open_control_plane(COMMAND, database_url, &local).await? {
        Ok(cp) => cp,
        Err(code) => return Ok(code),
    };
    match command {
        LibraryCommand::Add {
            slug,
            display_name,
            media_kind,
            description,
            disabled,
        } => {
            let input = NewLibrary {
                slug,
                display_name,
                media_kind: media_kind.to_store(),
                description,
                enabled: !disabled,
            };
            emit_library(cp.create_library(input).await, local)
        }
        LibraryCommand::List => list(&cp, local).await,
        LibraryCommand::Show { library_id } => show(&cp, local, LibraryId(library_id)).await,
        LibraryCommand::Update {
            library_id,
            display_name,
            media_kind,
            description,
        } => {
            let update = LibraryUpdate {
                display_name,
                media_kind: media_kind.map(LibraryMediaKindArg::to_store),
                description,
            };
            emit_library(
                cp.update_library(LibraryId(library_id), update).await,
                local,
            )
        }
        LibraryCommand::Enable { library_id } => emit_library(
            cp.set_library_enabled(LibraryId(library_id), true).await,
            local,
        ),
        LibraryCommand::Disable { library_id } => emit_library(
            cp.set_library_enabled(LibraryId(library_id), false).await,
            local,
        ),
        LibraryCommand::Remove { library_id } => remove(&cp, local, LibraryId(library_id)).await,
        LibraryCommand::SetDefaultScoringProfile {
            library_id,
            scoring_profile,
            clear,
        } => {
            let profile = if clear { None } else { scoring_profile };
            emit_library(
                cp.set_library_default_scoring_profile(LibraryId(library_id), profile.as_deref())
                    .await,
                local,
            )
        }
        LibraryCommand::Root(command) => root::run(&cp, local, command).await,
    }
}

fn emit_library(result: Result<Library, voom_core::VoomError>, local: Local) -> io::Result<i32> {
    match result {
        Ok(library) => {
            emit_ok(COMMAND, LibraryData::from(library), Some(local), Vec::new()).map(|()| 0)
        }
        Err(err) => emit_voom_error(COMMAND, &err, local),
    }
}

async fn list(cp: &ControlPlane, local: Local) -> io::Result<i32> {
    match cp.list_libraries().await {
        Ok(libraries) => {
            let data = ListData {
                libraries: libraries.into_iter().map(LibraryData::from).collect(),
            };
            emit_ok(COMMAND, data, Some(local), Vec::new()).map(|()| 0)
        }
        Err(err) => emit_voom_error(COMMAND, &err, local),
    }
}

async fn show(cp: &ControlPlane, local: Local, id: LibraryId) -> io::Result<i32> {
    match cp.get_library(id).await {
        Ok(Some(library)) => {
            emit_ok(COMMAND, LibraryData::from(library), Some(local), Vec::new()).map(|()| 0)
        }
        Ok(None) => not_found(local, id),
        Err(err) => emit_voom_error(COMMAND, &err, local),
    }
}

async fn remove(cp: &ControlPlane, local: Local, id: LibraryId) -> io::Result<i32> {
    match cp.delete_library(id).await {
        Ok(removed) => emit_ok(
            COMMAND,
            RemoveData {
                library_id: id.0,
                removed,
            },
            Some(local),
            Vec::new(),
        )
        .map(|()| 0),
        Err(err) => emit_voom_error(COMMAND, &err, local),
    }
}

fn not_found(local: Local, id: LibraryId) -> io::Result<i32> {
    emit_err(
        COMMAND,
        voom_core::ErrorCode::NotFound.as_str(),
        format!("library {id} not found"),
        None,
        Some(local),
    )?;
    Ok(2)
}
