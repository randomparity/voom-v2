use super::*;

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
    };
    let mut result = remux_result();
    result.kept_snapshot_stream_ids = Vec::new();

    let err = validate_result(&selection, &result).unwrap_err();

    assert_eq!(
        err.error_code(),
        voom_core::ErrorCode::MalformedWorkerResult
    );
    assert!(err.to_string().contains("kept stream ids"));
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
