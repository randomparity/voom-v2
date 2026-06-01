use super::*;

#[test]
fn node_and_worker_vocabs_use_stable_snake_case_wire_tokens() {
    assert_eq!(
        serde_json::to_string(&NodeKind::Remote).unwrap(),
        "\"remote\""
    );
    assert_eq!(
        serde_json::to_string(&NodeStatus::Registered).unwrap(),
        "\"registered\""
    );
    assert_eq!(
        serde_json::to_string(&WorkerKind::Synthetic).unwrap(),
        "\"synthetic\""
    );
    assert_eq!(
        serde_json::to_string(&WorkerStatus::Retired).unwrap(),
        "\"retired\""
    );
}

#[test]
fn database_parsers_reject_unknown_tokens_with_field_context() {
    let err = NodeKind::parse_database("nodes.kind", "edge").unwrap_err();
    assert!(err.to_string().contains("nodes.kind"));
    assert!(err.to_string().contains("edge"));

    let err = WorkerStatus::parse_database("workers.status", "paused").unwrap_err();
    assert!(err.to_string().contains("workers.status"));
    assert!(err.to_string().contains("paused"));
}
