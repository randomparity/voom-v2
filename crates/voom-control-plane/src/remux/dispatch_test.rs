use super::*;

use std::path::PathBuf;

use voom_store::repo::identity::{FileLocation, FileLocationKind, FileVersion, ProducedBy};
use voom_worker_protocol::{
    RemuxObservedFacts, RemuxResult, RemuxSelection, RemuxStatus, RemuxStreamRef, RemuxTrackGroup,
};

#[test]
fn validate_result_rejects_missing_kept_stream_id() {
    let selection = RemuxSelection {
        keep_streams: vec![RemuxStreamRef {
            snapshot_stream_id: "stream-0".to_owned(),
            provider_stream_index: 0,
        }],
        default_streams: Vec::new(),
        clear_default_streams: Vec::new(),
        track_order: vec![RemuxTrackGroup::Video],
        head_streams: Vec::new(),
        forced_streams: Vec::new(),
        clear_forced_streams: Vec::new(),
    };
    let mut result = remux_result();
    result.kept_snapshot_stream_ids = Vec::new();

    let err = validate_result(&selected_source(), &selection, &result).unwrap_err();

    assert_eq!(
        err.error_code(),
        voom_core::ErrorCode::MalformedWorkerResult
    );
    assert!(err.to_string().contains("kept stream ids"));
}

#[test]
fn validate_result_rejects_mismatched_default_stream_order() {
    let selection = RemuxSelection {
        keep_streams: vec![RemuxStreamRef {
            snapshot_stream_id: "stream-0".to_owned(),
            provider_stream_index: 0,
        }],
        default_streams: vec![RemuxStreamRef {
            snapshot_stream_id: "stream-0".to_owned(),
            provider_stream_index: 0,
        }],
        clear_default_streams: Vec::new(),
        track_order: vec![RemuxTrackGroup::Video],
        head_streams: Vec::new(),
        forced_streams: Vec::new(),
        clear_forced_streams: Vec::new(),
    };
    let mut result = remux_result();
    result.default_snapshot_stream_ids = Vec::new();

    let err = validate_result(&selected_source(), &selection, &result).unwrap_err();

    assert_eq!(
        err.error_code(),
        voom_core::ErrorCode::MalformedWorkerResult
    );
    assert!(err.to_string().contains("default stream ids"));
}

fn selected_source() -> crate::remux::source::SelectedSource {
    crate::remux::source::SelectedSource {
        version: FileVersion {
            id: voom_core::FileVersionId(1),
            file_asset_id: voom_core::FileAssetId(1),
            content_hash: "blake3:source".to_owned(),
            size_bytes: 12,
            produced_by: ProducedBy::Ingest,
            produced_from_version_id: None,
            created_at: time::OffsetDateTime::UNIX_EPOCH,
            retired_at: None,
            epoch: 0,
        },
        location: FileLocation {
            id: voom_core::FileLocationId(1),
            file_version_id: voom_core::FileVersionId(1),
            kind: FileLocationKind::LocalPath,
            value: "/library/source.mkv".to_owned(),
            proof_kind: None,
            proof_value: None,
            observed_at: time::OffsetDateTime::UNIX_EPOCH,
            retired_at: None,
            epoch: 0,
        },
        canonical_path: PathBuf::from("/library/source.mkv"),
    }
}

fn remux_result() -> RemuxResult {
    let input = RemuxObservedFacts {
        size_bytes: 12,
        content_hash: "blake3:source".to_owned(),
        modified_at: None,
        local_file_key: None,
    };
    RemuxResult {
        status: RemuxStatus::Remuxed,
        provider: "mkvtoolnix".to_owned(),
        provider_version: "test".to_owned(),
        input_pre: input.clone(),
        input_post: input,
        output: RemuxObservedFacts {
            size_bytes: 10,
            content_hash: "blake3:output".to_owned(),
            modified_at: None,
            local_file_key: None,
        },
        output_container: "mkv".to_owned(),
        kept_snapshot_stream_ids: vec!["stream-0".to_owned()],
        default_snapshot_stream_ids: Vec::new(),
    }
}
