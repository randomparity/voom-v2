use super::*;

#[test]
fn ticket_operation_accepts_known_operation_tokens() {
    assert_eq!(
        TicketOperation::new("synthetic.workflow.operation.hash_file")
            .unwrap()
            .as_str(),
        "synthetic.workflow.operation.hash_file"
    );
    assert_eq!(
        TicketOperation::from(OperationKind::ProbeFile).as_str(),
        "probe_file"
    );
}

#[test]
fn ticket_operation_rejects_empty_and_path_like_tokens() {
    assert!(TicketOperation::new("").is_err());
    assert!(TicketOperation::new("probe/file").is_err());
}
