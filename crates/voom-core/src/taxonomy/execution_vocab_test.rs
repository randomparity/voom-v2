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

#[test]
fn node_incarnation_vocabs_use_closed_stored_tokens() {
    let statuses = [
        (NodeIncarnationStatus::Active, "active"),
        (NodeIncarnationStatus::Superseded, "superseded"),
        (NodeIncarnationStatus::Retired, "retired"),
        (NodeIncarnationStatus::Failed, "failed"),
    ];
    for (status, token) in statuses {
        assert_eq!(status.as_str(), token);
        assert_eq!(
            NodeIncarnationStatus::parse_database("status", token).unwrap(),
            status
        );
    }

    let reasons = [
        (NodeIncarnationEndReason::Superseded, "superseded"),
        (
            NodeIncarnationEndReason::GracefulShutdown,
            "graceful_shutdown",
        ),
        (
            NodeIncarnationEndReason::ChildStartupFailed,
            "child_startup_failed",
        ),
        (
            NodeIncarnationEndReason::ChildRestartExhausted,
            "child_restart_exhausted",
        ),
        (
            NodeIncarnationEndReason::HeartbeatExpired,
            "heartbeat_expired",
        ),
        (
            NodeIncarnationEndReason::LogicalNodeRetired,
            "logical_node_retired",
        ),
    ];
    for (reason, token) in reasons {
        assert_eq!(reason.as_str(), token);
        assert_eq!(
            NodeIncarnationEndReason::parse_database("reason", token).unwrap(),
            reason
        );
    }

    assert!(NodeIncarnationStatus::parse_database("status", "paused").is_err());
    assert!(NodeIncarnationEndReason::parse_database("reason", "unknown").is_err());
}
