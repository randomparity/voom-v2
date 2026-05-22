use super::ticket_payload::WorkflowTicketPayload;
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
