use super::*;

#[test]
fn probe_request_uses_probe_file_and_deadlines() {
    let req = probe_request(voom_core::LeaseId(7), "/library/example.mkv");
    assert_eq!(req.operation, voom_worker_protocol::OperationKind::ProbeFile);
    assert_eq!(req.lease_id, voom_core::LeaseId(7));
    assert_eq!(req.heartbeat_deadline_ms, 1_000);
    assert_eq!(req.progress_idle_deadline_ms, 1_000);
}

#[test]
fn invalid_probe_request_omits_path() {
    let req = missing_path_request(voom_core::LeaseId(8));
    assert_eq!(req.operation, voom_worker_protocol::OperationKind::ProbeFile);
    assert!(req.payload.get("path").is_none());
}
