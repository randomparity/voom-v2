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
