use std::io;

use serde::Serialize;
use voom_control_plane::ControlPlane;
use voom_core::{BundleId, ErrorCode, VoomError, format_iso8601};
use voom_store::repo::bundles::{AssetBundle, BundleMember};
use voom_store::repo::identity::{
    FileLocation, FileLocationKind, FileVersion, MediaVariant, MediaWork,
};

use crate::cli::BundleCommand;
use crate::commands::common::{emit_voom_error, next_cursor, open_control_plane};
use crate::envelope::{Local, emit_err, emit_ok, emit_ok_page};

const COMMAND: &str = "bundle";

#[derive(Debug, Serialize)]
struct ListData {
    bundles: Vec<BundleSummaryData>,
}

#[derive(Debug, Serialize)]
struct BundleSummaryData {
    id: u64,
    media_variant_id: u64,
    display_name: String,
    created_at: String,
    member_count: u64,
}

#[derive(Debug, Serialize)]
struct ShowData {
    bundle: BundleData,
    lineage: LineageData,
    members: Vec<MemberData>,
}

#[derive(Debug, Serialize)]
struct BundleData {
    id: u64,
    media_variant_id: u64,
    display_name: String,
    created_at: String,
    epoch: u64,
}

#[derive(Debug, Serialize)]
struct LineageData {
    media_variant: Option<VariantData>,
    media_work: Option<WorkData>,
}

#[derive(Debug, Serialize)]
struct VariantData {
    id: u64,
    media_work_id: u64,
    label: String,
    provisional: bool,
}

#[derive(Debug, Serialize)]
struct WorkData {
    id: u64,
    kind: String,
    display_title: String,
    provisional: bool,
}

#[derive(Debug, Serialize)]
struct MemberData {
    file_asset_id: u64,
    role: &'static str,
    file_version_id: Option<u64>,
    content_hash: Option<String>,
    size_bytes: Option<u64>,
    produced_by: Option<String>,
    produced_from_version_id: Option<u64>,
    location: Option<String>,
}

pub async fn run(database_url: &str, local: Local, command: BundleCommand) -> io::Result<i32> {
    match command {
        BundleCommand::List { limit, after_id } => list(database_url, local, limit, after_id).await,
        BundleCommand::Show { bundle_id } => show(database_url, local, bundle_id).await,
    }
}

async fn list(
    database_url: &str,
    local: Local,
    limit: u32,
    after_id: Option<u64>,
) -> io::Result<i32> {
    let cp = match open_control_plane(COMMAND, database_url, &local).await? {
        Ok(cp) => cp,
        Err(code) => return Ok(code),
    };
    match cp.list_bundles(after_id, limit).await {
        Ok(rows) => {
            let cursor = next_cursor(&rows, limit, |(bundle, _)| bundle.id.0);
            emit_ok_page(
                COMMAND,
                ListData {
                    bundles: rows.into_iter().map(bundle_summary).collect(),
                },
                cursor,
                Some(local),
                Vec::new(),
            )
            .map(|()| 0)
        }
        Err(err) => emit_voom_error(COMMAND, &err, local),
    }
}

async fn show(database_url: &str, local: Local, bundle_id: u64) -> io::Result<i32> {
    let cp = match open_control_plane(COMMAND, database_url, &local).await? {
        Ok(cp) => cp,
        Err(code) => return Ok(code),
    };
    match load_show(&cp, BundleId(bundle_id)).await {
        Ok(Some(data)) => emit_ok(COMMAND, data, Some(local), Vec::new()).map(|()| 0),
        Ok(None) => {
            emit_err(
                COMMAND,
                ErrorCode::NotFound.as_str(),
                format!("bundle show: id={bundle_id} not found"),
                None,
                Some(local),
            )?;
            Ok(2)
        }
        Err(err) => emit_voom_error(COMMAND, &err, local),
    }
}

async fn load_show(cp: &ControlPlane, id: BundleId) -> Result<Option<ShowData>, VoomError> {
    let Some(bundle) = cp.get_bundle(id).await? else {
        return Ok(None);
    };
    let variant = cp.get_media_variant(bundle.media_variant_id).await?;
    let work = match &variant {
        Some(variant) => cp.get_media_work(variant.media_work_id).await?,
        None => None,
    };
    let member_rows = cp.list_bundle_members(bundle.id).await?;
    let mut members = Vec::with_capacity(member_rows.len());
    for member in member_rows {
        members.push(member_data(cp, member).await?);
    }
    Ok(Some(ShowData {
        bundle: BundleData {
            id: bundle.id.0,
            media_variant_id: bundle.media_variant_id.0,
            display_name: bundle.display_name,
            created_at: format_iso8601(bundle.created_at),
            epoch: bundle.epoch,
        },
        lineage: LineageData {
            media_variant: variant.map(variant_data),
            media_work: work.map(work_data),
        },
        members,
    }))
}

async fn member_data(cp: &ControlPlane, member: BundleMember) -> Result<MemberData, VoomError> {
    // Provenance is read from the member's single live file version — highest
    // id if several are live; all version-derived fields are null if none is.
    let live_version =
        select_live_version(cp.list_file_versions_by_asset(member.file_asset_id).await?);

    let mut member_data = MemberData {
        file_asset_id: member.file_asset_id.0,
        role: member.role.as_str(),
        file_version_id: None,
        content_hash: None,
        size_bytes: None,
        produced_by: None,
        produced_from_version_id: None,
        location: None,
    };
    if let Some(version) = live_version {
        let location =
            select_local_location(cp.list_live_file_locations_by_version(version.id).await?);
        member_data.file_version_id = Some(version.id.0);
        member_data.content_hash = Some(version.content_hash);
        member_data.size_bytes = Some(version.size_bytes);
        member_data.produced_by = Some(version.produced_by.as_str().to_owned());
        member_data.produced_from_version_id = version.produced_from_version_id.map(|id| id.0);
        member_data.location = location;
    }
    Ok(member_data)
}

/// Pick the member's single live file version — highest id when several are
/// live, `None` when none is (per the `bundle show` contract).
fn select_live_version(versions: Vec<FileVersion>) -> Option<FileVersion> {
    versions
        .into_iter()
        .filter(|version| version.retired_at.is_none())
        .max_by_key(|version| version.id.0)
}

/// Pick the live local-path location (highest id when several), else `None`.
fn select_local_location(locations: Vec<FileLocation>) -> Option<String> {
    locations
        .into_iter()
        .filter(|location| location.kind == FileLocationKind::LocalPath)
        .max_by_key(|location| location.id.0)
        .map(|location| location.value)
}

fn bundle_summary((bundle, member_count): (AssetBundle, u64)) -> BundleSummaryData {
    BundleSummaryData {
        id: bundle.id.0,
        media_variant_id: bundle.media_variant_id.0,
        display_name: bundle.display_name,
        created_at: format_iso8601(bundle.created_at),
        member_count,
    }
}

fn variant_data(variant: MediaVariant) -> VariantData {
    VariantData {
        id: variant.id.0,
        media_work_id: variant.media_work_id.0,
        label: variant.label,
        provisional: variant.provisional,
    }
}

fn work_data(work: MediaWork) -> WorkData {
    WorkData {
        id: work.id.0,
        kind: work.kind.as_str().to_owned(),
        display_title: work.display_title,
        provisional: work.provisional,
    }
}

#[cfg(test)]
#[path = "bundle_test.rs"]
mod tests;
