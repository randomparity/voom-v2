use super::*;

#[tokio::test]
async fn wrong_lease_fixture_is_rejected() {
    let err = classify_fixture(FixtureMode::WrongLeaseId)
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        voom_worker_protocol::ProtocolError::WrongLeaseId { .. }
    ));
}

#[tokio::test]
async fn frame_after_terminal_fixture_is_rejected() {
    let bytes = fixture_bytes(FixtureMode::FrameAfterTerminal, voom_core::LeaseId(1)).unwrap();
    assert!(has_frame_after_terminal(&bytes, voom_core::LeaseId(1)).unwrap());
    let err = classify_fixture(FixtureMode::FrameAfterTerminal)
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        voom_worker_protocol::ProtocolError::UnexpectedFrameAfterTerminal
    ));
}

#[tokio::test]
async fn truncated_fixture_is_malformed() {
    let err = classify_fixture(FixtureMode::TruncatedBody)
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        voom_worker_protocol::ProtocolError::MalformedFrame { .. }
    ));
}
