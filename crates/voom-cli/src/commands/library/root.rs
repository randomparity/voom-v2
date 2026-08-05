//! `voom library root`: administer durable node-owned storage roots.

use std::io;

use serde::Serialize;
use voom_control_plane::ControlPlane;
use voom_core::{LibraryId, NodeId, ProviderLocator, StorageRootId, VoomError, format_iso8601};
use voom_store::repo::library::library_roots::{
    EffectiveLibraryRoot, LibraryRoot, LibraryRootUpdate, NewLibraryRoot,
};

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
    pub owner_node_id: Option<u64>,
    pub provider_kind: String,
    pub provider_locator: String,
    pub display_locator: String,
    pub state: String,
    pub root_epoch: u64,
    pub activation_identity: Option<String>,
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
    pub default_output_root_id: Option<u64>,
    pub default_staging_root_id: Option<u64>,
    pub default_backup_root_id: Option<u64>,
    pub enabled: bool,
    pub effectively_available: bool,
    pub availability_reason: String,
    pub created_at: String,
    pub updated_at: String,
}

impl From<EffectiveLibraryRoot> for LibraryRootData {
    fn from(effective: EffectiveLibraryRoot) -> Self {
        let root = effective.root;
        Self {
            root_id: root.id.0,
            library_id: root.library_id.0,
            owner_node_id: root.owner_node_id.map(|id| id.0),
            provider_kind: root.provider_kind.as_str().to_owned(),
            provider_locator: root.provider_locator.into_inner(),
            display_locator: root.display_locator,
            state: root.state.as_str().to_owned(),
            root_epoch: root.root_epoch,
            activation_identity: root.activation_identity,
            include_globs: root.include_globs,
            exclude_globs: root.exclude_globs,
            extension_allowlist: root.extension_allowlist,
            scan_mode: root.scan_mode.as_str().to_owned(),
            symlink_policy: root.symlink_policy.as_str().to_owned(),
            hidden_file_policy: root.hidden_file_policy.as_str().to_owned(),
            max_depth: root.max_depth,
            stability_seconds: root.stability_seconds,
            debounce_seconds: root.debounce_seconds,
            default_output_root_id: root.default_output_root_id.map(|id| id.0),
            default_staging_root_id: root.default_staging_root_id.map(|id| id.0),
            default_backup_root_id: root.default_backup_root_id.map(|id| id.0),
            enabled: root.enabled,
            effectively_available: effective.available,
            availability_reason: effective.reason.as_str().to_owned(),
            created_at: format_iso8601(root.created_at),
            updated_at: format_iso8601(root.updated_at),
        }
    }
}

#[derive(Debug, Serialize)]
struct ListData {
    roots: Vec<LibraryRootData>,
}

pub async fn run(cp: &ControlPlane, local: Local, command: LibraryRootCommand) -> io::Result<i32> {
    match command {
        LibraryRootCommand::Add(args) => add(cp, local, args).await,
        LibraryRootCommand::List { library_id } => list(cp, local, library_id.map(LibraryId)).await,
        LibraryRootCommand::Show { root_id } => show(cp, local, StorageRootId(root_id)).await,
        LibraryRootCommand::Update(args) => update(cp, local, args).await,
        LibraryRootCommand::Enable { root_id } => {
            emit_root(
                cp,
                cp.set_library_root_enabled(StorageRootId(root_id), true)
                    .await,
                local,
            )
            .await
        }
        LibraryRootCommand::Disable { root_id } => {
            emit_root(
                cp,
                cp.set_library_root_enabled(StorageRootId(root_id), false)
                    .await,
                local,
            )
            .await
        }
        LibraryRootCommand::AssignOwner {
            root_id,
            owner_node_id,
        } => {
            emit_root(
                cp,
                cp.assign_library_root_owner(StorageRootId(root_id), NodeId(owner_node_id))
                    .await,
                local,
            )
            .await
        }
        LibraryRootCommand::Retire { root_id } => {
            emit_root(
                cp,
                cp.retire_library_root(StorageRootId(root_id)).await,
                local,
            )
            .await
        }
    }
}

async fn add(cp: &ControlPlane, local: Local, args: LibraryRootAddArgs) -> io::Result<i32> {
    let provider_locator = match ProviderLocator::new(args.provider_locator) {
        Ok(locator) => locator,
        Err(error) => return emit_voom_error(COMMAND, &error, local),
    };
    let display_locator = args
        .display_locator
        .unwrap_or_else(|| provider_locator.as_str().to_owned());
    let input = NewLibraryRoot {
        library_id: LibraryId(args.library_id),
        owner_node_id: NodeId(args.owner_node_id),
        provider_kind: args.provider.to_store(),
        provider_locator,
        display_locator,
        include_globs: args.include_glob,
        exclude_globs: args.exclude_glob,
        extension_allowlist: args.extension,
        scan_mode: args.scan_mode.to_store(),
        symlink_policy: args.symlink_policy.to_store(),
        hidden_file_policy: args.hidden_file_policy.to_store(),
        max_depth: args.max_depth,
        stability_seconds: args.stability_seconds,
        debounce_seconds: args.debounce_seconds,
        default_output_root_id: args.output_root.map(StorageRootId),
        default_staging_root_id: args.staging_root.map(StorageRootId),
        default_backup_root_id: args.backup_root.map(StorageRootId),
        enabled: !args.disabled,
    };
    emit_root(cp, cp.create_library_root(input).await, local).await
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
        default_output_root_id: args.output_root.map(|id| Some(StorageRootId(id))),
        default_staging_root_id: args.staging_root.map(|id| Some(StorageRootId(id))),
        default_backup_root_id: args.backup_root.map(|id| Some(StorageRootId(id))),
    };
    emit_root(
        cp,
        cp.update_library_root(StorageRootId(args.root_id), update)
            .await,
        local,
    )
    .await
}

async fn list(cp: &ControlPlane, local: Local, library_id: Option<LibraryId>) -> io::Result<i32> {
    let roots = match cp.list_library_roots(library_id).await {
        Ok(roots) => roots,
        Err(error) => return emit_voom_error(COMMAND, &error, local),
    };
    let mut data = Vec::with_capacity(roots.len());
    for root in roots {
        match cp.effective_library_root(root.id).await {
            Ok(Some(effective)) => data.push(LibraryRootData::from(effective)),
            Ok(None) => {
                return emit_voom_error(
                    COMMAND,
                    &VoomError::Internal(format!("library root {} vanished", root.id)),
                    local,
                );
            }
            Err(error) => return emit_voom_error(COMMAND, &error, local),
        }
    }
    emit_ok(COMMAND, ListData { roots: data }, Some(local), Vec::new()).map(|()| 0)
}

async fn show(cp: &ControlPlane, local: Local, id: StorageRootId) -> io::Result<i32> {
    match cp.effective_library_root(id).await {
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
                format!("storage root {id} not found"),
                None,
                Some(local),
            )?;
            Ok(2)
        }
        Err(error) => emit_voom_error(COMMAND, &error, local),
    }
}

async fn emit_root(
    cp: &ControlPlane,
    result: Result<LibraryRoot, VoomError>,
    local: Local,
) -> io::Result<i32> {
    let root = match result {
        Ok(root) => root,
        Err(error) => return emit_voom_error(COMMAND, &error, local),
    };
    match cp.effective_library_root(root.id).await {
        Ok(Some(effective)) => emit_ok(
            COMMAND,
            LibraryRootData::from(effective),
            Some(local),
            Vec::new(),
        )
        .map(|()| 0),
        Ok(None) => emit_voom_error(
            COMMAND,
            &VoomError::Internal(format!("library root {} vanished", root.id)),
            local,
        ),
        Err(error) => emit_voom_error(COMMAND, &error, local),
    }
}
