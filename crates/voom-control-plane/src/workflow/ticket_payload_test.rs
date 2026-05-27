use crate::workflow::ticket_payload::WorkflowTicketPayload;
use voom_worker_protocol::OperationKind;

#[test]
fn workflow_ticket_payload_rejects_operation_mismatch() {
    let payload = WorkflowTicketPayload::new_for_test(
        "workflow-1",
        "plan-1",
        "probe",
        "file-000",
        OperationKind::ProbeFile,
        serde_json::json!({"path": "/library/file-000.mkv"}),
    );
    let encoded = payload.to_ticket_payload().unwrap();
    let err =
        WorkflowTicketPayload::parse_ticket("synthetic.workflow.operation.scan_library", encoded)
            .unwrap_err();
    assert!(err.to_string().contains("operation mismatch"));
}

#[test]
fn workflow_ticket_payload_rejects_rendered_operation_mismatch_on_encode() {
    let payload = WorkflowTicketPayload::new_for_test(
        "workflow-1",
        "plan-1",
        "probe",
        "file-000",
        OperationKind::ProbeFile,
        serde_json::json!({
            "operation": "scan_library",
            "path": "/library/file-000.mkv"
        }),
    );

    let err = payload.to_ticket_payload().unwrap_err();
    assert!(err.to_string().contains("operation mismatch"));
}

#[test]
fn workflow_ticket_payload_accepts_transcode_audio_operation_name() {
    let payload = WorkflowTicketPayload::new_for_test(
        "workflow-1",
        "plan-1",
        "audio",
        "file-000",
        OperationKind::TranscodeAudio,
        serde_json::json!({
            "operation": "transcode_audio",
            "path": "/library/file-000.mkv"
        }),
    );

    let encoded = payload.to_ticket_payload().unwrap();
    let parsed = WorkflowTicketPayload::parse_ticket(
        "synthetic.workflow.operation.transcode_audio",
        encoded,
    )
    .unwrap();

    assert_eq!(parsed.operation, OperationKind::TranscodeAudio);
}
