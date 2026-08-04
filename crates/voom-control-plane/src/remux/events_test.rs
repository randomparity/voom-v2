use super::*;

#[test]
fn stream_payloads_preserve_snapshot_ids_and_provider_indexes() {
    let streams = vec![RemuxStreamRef {
        snapshot_stream_id: "stream-1".to_owned(),
        provider_stream_index: 7,
    }];

    let payloads = stream_payloads(&streams);

    assert_eq!(payloads[0].snapshot_stream_id, "stream-1");
    assert_eq!(payloads[0].provider_stream_index, 7);
}

#[test]
fn remux_succeeded_payload_preserves_every_distinct_domain_id() {
    let input = ExecuteRemuxInput {
        job_id: voom_core::JobId(101),
        ticket_id: voom_core::TicketId(102),
        lease_id: voom_core::LeaseId(103),
        source_file_version_id: voom_core::FileVersionId(104),
        source_location_id: None,
        operation_payload: serde_json::json!({}),
        staging_root: std::path::PathBuf::from("/staging"),
        target_dir: std::path::PathBuf::from("/target"),
        backup_root: None,
    };
    let selection = RemuxSelection {
        keep_streams: Vec::new(),
        default_streams: Vec::new(),
        clear_default_streams: Vec::new(),
        track_order: Vec::new(),
        head_streams: Vec::new(),
        forced_streams: Vec::new(),
        clear_forced_streams: Vec::new(),
    };
    let facts = voom_worker_protocol::RemuxObservedFacts {
        size_bytes: 1,
        content_hash: "blake3:test".to_owned(),
        modified_at: None,
        local_file_key: None,
    };
    let result = RemuxResult {
        status: voom_worker_protocol::RemuxStatus::Remuxed,
        provider: "mkvtoolnix".to_owned(),
        provider_version: "test".to_owned(),
        input_pre: facts.clone(),
        input_post: facts.clone(),
        output: facts,
        output_container: "mkv".to_owned(),
        kept_snapshot_stream_ids: Vec::new(),
        default_snapshot_stream_ids: Vec::new(),
    };
    let event = RemuxSucceededEvent::from_input(&RemuxSucceededEventInput {
        input: &input,
        source_location_id: voom_core::FileLocationId(105),
        selection: &selection,
        staging_path: std::path::Path::new("/staging/output.mkv"),
        artifact_handle_id: voom_core::ArtifactHandleId(106),
        artifact_location_id: voom_core::ArtifactLocationId(107),
        result: &result,
    });

    let payload = event.payload();
    assert_eq!(payload.job_id, voom_core::JobId(101));
    assert_eq!(payload.ticket_id, voom_core::TicketId(102));
    assert_eq!(payload.lease_id, Some(voom_core::LeaseId(103)));
    assert_eq!(
        payload.source_file_version_id,
        voom_core::FileVersionId(104)
    );
    assert_eq!(
        payload.source_file_location_id,
        voom_core::FileLocationId(105)
    );
    assert_eq!(payload.artifact_handle_id, voom_core::ArtifactHandleId(106));
    assert_eq!(
        payload.artifact_location_id,
        voom_core::ArtifactLocationId(107)
    );
}
