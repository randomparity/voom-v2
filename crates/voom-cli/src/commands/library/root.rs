//! `voom library root <add|list|show|update|enable|disable|remove>`.

use std::io;
use std::path::Path;

use serde::Serialize;
use voom_control_plane::ControlPlane;
use voom_core::{LibraryId, LibraryRootId, VoomError, format_iso8601};
use voom_store::repo::library::library_roots::{LibraryRoot, LibraryRootUpdate, NewLibraryRoot};

use crate::cli::{
    HiddenFilePolicyArg, LibraryRootAddArgs, LibraryRootCommand, LibraryRootUpdateArgs,
    LibraryScanModeArg, SymlinkPolicyArg,
};
use crate::commands::common::emit_voom_error;
use crate::envelope::{Local, emit_err, emit_ok};

use super::COMMAND;

#[derive(Debug, Serialize)]
pub struct LibraryRootData {
    pub root_id: u64,
    pub library_id: u64,
    pub root_kind: String,
    pub canonical_path: String,
    pub display_path: String,
    pub include_globs: Vec<String>,
    pub exclude_globs: Vec<String>,
    pub extension_allowlist: Vec<String>,
    pub scan_mode: String,
    pub symlink_policy: String,
    pub hidden_file_policy: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_depth: Option<u32>,
    pub stability_seconds: u32,
    pub debounce_seconds: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_output_root: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_staging_root: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_backup_root: Option<String>,
    pub enabled: bool,
    pub created_at: String,
    pub updated_at: String,
}

impl From<LibraryRoot> for LibraryRootData {
    fn from(root: LibraryRoot) -> Self {
        Self {
            root_id: root.id.0,
            library_id: root.library_id.0,
            root_kind: root.root_kind.as_str().to_owned(),
            canonical_path: root.canonical_path,
            display_path: root.display_path,
            include_globs: root.include_globs,
            exclude_globs: root.exclude_globs,
            extension_allowlist: root.extension_allowlist,
            scan_mode: root.scan_mode.as_str().to_owned(),
            symlink_policy: root.symlink_policy.as_str().to_owned(),
            hidden_file_policy: root.hidden_file_policy.as_str().to_owned(),
            max_depth: root.max_depth,
            stability_seconds: root.stability_seconds,
            debounce_seconds: root.debounce_seconds,
            default_output_root: root.default_output_root,
            default_staging_root: root.default_staging_root,
            default_backup_root: root.default_backup_root,
            enabled: root.enabled,
            created_at: format_iso8601(root.created_at),
            updated_at: format_iso8601(root.updated_at),
        }
    }
}

#[derive(Debug, Serialize)]
struct ListData {
    roots: Vec<LibraryRootData>,
}

#[derive(Debug, Serialize)]
struct RemoveData {
    root_id: u64,
    removed: bool,
}

pub async fn run(cp: &ControlPlane, local: Local, command: LibraryRootCommand) -> io::Result<i32> {
    match command {
        LibraryRootCommand::Add(args) => add(cp, local, args).await,
        LibraryRootCommand::List { library_id } => list(cp, local, library_id.map(LibraryId)).await,
        LibraryRootCommand::Show { root_id } => show(cp, local, LibraryRootId(root_id)).await,
        LibraryRootCommand::Update(args) => update(cp, local, args).await,
        LibraryRootCommand::Enable { root_id } => emit_root(
            cp.set_library_root_enabled(LibraryRootId(root_id), true)
                .await,
            local,
        ),
        LibraryRootCommand::Disable { root_id } => emit_root(
            cp.set_library_root_enabled(LibraryRootId(root_id), false)
                .await,
            local,
        ),
        LibraryRootCommand::Remove { root_id } => remove(cp, local, LibraryRootId(root_id)).await,
    }
}

async fn add(cp: &ControlPlane, local: Local, args: LibraryRootAddArgs) -> io::Result<i32> {
    let (canonical_path, display_path) = match canonicalize_root_path(&args.path).await {
        Ok(paths) => paths,
        Err(message) => {
            emit_err(
                COMMAND,
                voom_core::ErrorCode::BadArgs.as_str(),
                message,
                None,
                Some(local),
            )?;
            return Ok(1);
        }
    };
    let input = NewLibraryRoot {
        library_id: LibraryId(args.library_id),
        root_kind: args.root_kind.to_store(),
        canonical_path,
        display_path,
        include_globs: args.include_glob,
        exclude_globs: args.exclude_glob,
        extension_allowlist: args.extension,
        scan_mode: args.scan_mode.to_store(),
        symlink_policy: args.symlink_policy.to_store(),
        hidden_file_policy: args.hidden_file_policy.to_store(),
        max_depth: args.max_depth,
        stability_seconds: args.stability_seconds,
        debounce_seconds: args.debounce_seconds,
        default_output_root: args.output_root,
        default_staging_root: args.staging_root,
        default_backup_root: args.backup_root,
        enabled: !args.disabled,
    };
    emit_root(cp.create_library_root(input).await, local)
}

async fn update(cp: &ControlPlane, local: Local, args: LibraryRootUpdateArgs) -> io::Result<i32> {
    let update = LibraryRootUpdate {
        include_globs: args.include_glob,
        exclude_globs: args.exclude_glob,
        extension_allowlist: args.extension,
        scan_mode: args.scan_mode.map(LibraryScanModeArg::to_store),
        symlink_policy: args.symlink_policy.map(SymlinkPolicyArg::to_store),
        hidden_file_policy: args.hidden_file_policy.map(HiddenFilePolicyArg::to_store),
        max_depth: args.max_depth,
        stability_seconds: args.stability_seconds,
        debounce_seconds: args.debounce_seconds,
        default_output_root: args.output_root,
        default_staging_root: args.staging_root,
        default_backup_root: args.backup_root,
    };
    emit_root(
        cp.update_library_root(LibraryRootId(args.root_id), update)
            .await,
        local,
    )
}

async fn list(cp: &ControlPlane, local: Local, library_id: Option<LibraryId>) -> io::Result<i32> {
    match cp.list_library_roots(library_id).await {
        Ok(roots) => {
            let data = ListData {
                roots: roots.into_iter().map(LibraryRootData::from).collect(),
            };
            emit_ok(COMMAND, data, Some(local), Vec::new()).map(|()| 0)
        }
        Err(err) => emit_voom_error(COMMAND, &err, local),
    }
}

async fn show(cp: &ControlPlane, local: Local, id: LibraryRootId) -> io::Result<i32> {
    match cp.get_library_root(id).await {
        Ok(Some(root)) => emit_ok(
            COMMAND,
            LibraryRootData::from(root),
            Some(local),
            Vec::new(),
        )
        .map(|()| 0),
        Ok(None) => {
            emit_err(
                COMMAND,
                voom_core::ErrorCode::NotFound.as_str(),
                format!("library root {id} not found"),
                None,
                Some(local),
            )?;
            Ok(2)
        }
        Err(err) => emit_voom_error(COMMAND, &err, local),
    }
}

async fn remove(cp: &ControlPlane, local: Local, id: LibraryRootId) -> io::Result<i32> {
    match cp.delete_library_root(id).await {
        Ok(removed) => emit_ok(
            COMMAND,
            RemoveData {
                root_id: id.0,
                removed,
            },
            Some(local),
            Vec::new(),
        )
        .map(|()| 0),
        Err(err) => emit_voom_error(COMMAND, &err, local),
    }
}

fn emit_root(result: Result<LibraryRoot, VoomError>, local: Local) -> io::Result<i32> {
    match result {
        Ok(root) => emit_ok(
            COMMAND,
            LibraryRootData::from(root),
            Some(local),
            Vec::new(),
        )
        .map(|()| 0),
        Err(err) => emit_voom_error(COMMAND, &err, local),
    }
}

/// Canonicalize a root path and reject a symlinked leaf. Returns
/// `(canonical_path, display_path)`. Storing the canonical path is what defeats
/// alias-escape (ADR 0027).
async fn canonicalize_root_path(path: &Path) -> Result<(String, String), String> {
    let metadata = tokio::fs::symlink_metadata(path)
        .await
        .map_err(|err| format!("cannot inspect root path {}: {err}", path.display()))?;
    if metadata.file_type().is_symlink() {
        return Err(format!(
            "root path must not be a symlink: {}",
            path.display()
        ));
    }
    let canonical = tokio::fs::canonicalize(path)
        .await
        .map_err(|err| format!("cannot canonicalize root path {}: {err}", path.display()))?;
    let canonical_path = canonical
        .to_str()
        .ok_or_else(|| format!("root path is not valid UTF-8: {}", canonical.display()))?
        .to_owned();
    let display_path = path
        .to_str()
        .ok_or_else(|| format!("root path is not valid UTF-8: {}", path.display()))?
        .to_owned();
    Ok((canonical_path, display_path))
}
