use super::*;

use time::OffsetDateTime;
use voom_core::{FileAssetId, FileLocationId, FileVersionId};
use voom_store::repo::identity::ProducedBy;

const EPOCH: OffsetDateTime = OffsetDateTime::UNIX_EPOCH;

fn version(id: u64, retired: bool) -> FileVersion {
    FileVersion {
        id: FileVersionId(id),
        file_asset_id: FileAssetId(1),
        content_hash: format!("sha256:{id}"),
        size_bytes: id,
        produced_by: ProducedBy::Ingest,
        produced_from_version_id: None,
        created_at: EPOCH,
        retired_at: retired.then_some(EPOCH),
        epoch: 0,
    }
}

fn location(id: u64, kind: FileLocationKind, value: &str) -> FileLocation {
    FileLocation {
        id: FileLocationId(id),
        file_version_id: FileVersionId(1),
        kind,
        value: value.to_owned(),
        proof_kind: None,
        proof_value: None,
        observed_at: EPOCH,
        retired_at: None,
        epoch: 0,
    }
}

#[test]
fn select_live_version_picks_highest_id_among_live() {
    let chosen = select_live_version(vec![
        version(1, false),
        version(3, true), // retired: excluded even though highest id
        version(2, false),
    ]);
    assert_eq!(chosen.map(|version| version.id.0), Some(2));
}

#[test]
fn select_live_version_is_none_when_empty_or_all_retired() {
    assert!(select_live_version(Vec::new()).is_none());
    assert!(select_live_version(vec![version(1, true), version(2, true)]).is_none());
}

#[test]
fn select_local_location_picks_highest_id_local_path() {
    let chosen = select_local_location(vec![
        location(1, FileLocationKind::LocalPath, "/a"),
        location(5, FileLocationKind::ObjectStoreKey, "s3://ignored"),
        location(3, FileLocationKind::LocalPath, "/b"),
    ]);
    assert_eq!(chosen, Some("/b".to_owned()));
}

#[test]
fn select_local_location_is_none_without_a_local_path() {
    assert!(select_local_location(Vec::new()).is_none());
    assert!(
        select_local_location(vec![location(1, FileLocationKind::BackupPath, "/backup")]).is_none()
    );
}
