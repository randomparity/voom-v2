use voom_core::WorkerId;
use voom_worker_protocol::WorkerCredentials;

use super::*;

#[test]
fn worker_server_uses_supplied_credentials() {
    let credentials = WorkerCredentials {
        worker_id: WorkerId(13),
        worker_epoch: 17,
        secret: "test-secret".to_owned().into(),
    };

    let server = worker_server(credentials);

    assert_eq!(server.credentials.worker_id, WorkerId(13));
    assert_eq!(server.credentials.worker_epoch, 17);
}
