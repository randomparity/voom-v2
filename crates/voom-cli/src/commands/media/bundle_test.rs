use super::*;

use time::OffsetDateTime;
use voom_core::{
    FileAssetId, FileLocationId, FileVersionId, ProviderRelativeLocator, StorageRootId,
};
use voom_store::repo::media::identity::{FileLocationAddress, ProducedBy};

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

fn rooted_location(id: u64, relative_locator: &str) -> FileLocation {
    FileLocation {
        id: FileLocationId(id),
        file_version_id: FileVersionId(1),
        address: FileLocationAddress::Rooted {
            storage_root_id: StorageRootId(7),
            provider_relative_locator: ProviderRelativeLocator::new(relative_locator.to_owned())
                .unwrap(),
        },
        proof_kind: None,
        proof_value: None,
        observed_at: EPOCH,
        retired_at: None,
        epoch: 0,
    }
}

fn legacy_location(id: u64) -> FileLocation {
    FileLocation {
        id: FileLocationId(id),
        file_version_id: FileVersionId(1),
        address: FileLocationAddress::UnassignedLegacy {
            legacy_kind: "object_store_key".to_owned(),
            legacy_locator: "s3://ignored".to_owned(),
        },
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
fn select_local_location_picks_highest_id_rooted_location() {
    let chosen = select_local_location(vec![
        rooted_location(1, "a"),
        legacy_location(5),
        rooted_location(3, "b"),
    ]);
    assert_eq!(
        chosen,
        Some(LocationData {
            file_location_id: 3,
            storage_root_id: 7,
            provider_relative_locator: "b".to_owned(),
        })
    );
}

#[test]
fn select_local_location_is_none_without_a_rooted_location() {
    assert!(select_local_location(Vec::new()).is_none());
    assert!(select_local_location(vec![legacy_location(1)]).is_none());
}
