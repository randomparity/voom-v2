use voom_core::WorkerId;
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
