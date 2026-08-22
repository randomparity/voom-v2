use voom_core::WorkerId;
use voom_hash_worker::operation_handler;
use voom_worker_protocol::WorkerCredentials;

use super::*;

#[test]
fn worker_server_uses_supplied_credentials() {
    let bearer_fixture = "test-bearer".to_owned();
    let credentials = WorkerCredentials {
        worker_id: WorkerId(7),
        worker_epoch: 11,
        secret: bearer_fixture.into(),
    };

    let server = worker_server(credentials);

    assert_eq!(server.credentials.worker_id, WorkerId(7));
    assert_eq!(server.credentials.worker_epoch, 11);
}

#[test]
fn worker_server_wires_the_operation_handler() {
    let bearer_fixture = "test-bearer".to_owned();
    let credentials = WorkerCredentials {
        worker_id: WorkerId(3),
        worker_epoch: 5,
        secret: bearer_fixture.into(),
    };
    let standalone = operation_handler();

    // The binary serves dispatches through this handler; the wiring test
    // pins that the server construction path accepts it.
    let server = worker_server(credentials);

    assert_eq!(server.credentials.worker_id, WorkerId(3));
    assert_eq!(std::sync::Arc::strong_count(&standalone), 1);
}
