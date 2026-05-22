use std::sync::Arc;

use async_trait::async_trait;
use secrecy::SecretString;
use voom_core::WorkerId;
use voom_worker_protocol::{
    ClientHandle, DispatchStream, OperationRequest, ProtocolError, WorkerCredentials,
};

use crate::workflow::runtime::WorkerRuntimeRegistry;

#[tokio::test]
async fn registry_returns_registered_in_process_runtime() {
    let worker_id = WorkerId(42);
    let client = Arc::new(NoopClient);
    let credentials = credentials(worker_id);
    let registry = WorkerRuntimeRegistry::new().with_in_process_runtime(
        worker_id,
        client.clone(),
        credentials.clone(),
    );

    let runtime = registry.get(worker_id).unwrap();

    assert!(Arc::strong_count(&client) >= 2);
    assert_eq!(runtime.credentials.worker_id, credentials.worker_id);
    assert_eq!(runtime.credentials.worker_epoch, credentials.worker_epoch);
}

#[tokio::test]
async fn registry_reports_missing_worker_runtime() {
    let registry = WorkerRuntimeRegistry::new();

    let err = registry.get(WorkerId(99)).unwrap_err();

    assert!(err.to_string().contains("runtime for worker 99"));
}

fn credentials(worker_id: WorkerId) -> WorkerCredentials {
    WorkerCredentials {
        worker_id,
        worker_epoch: 0,
        secret: SecretString::from("test-secret"),
    }
}

#[derive(Debug)]
struct NoopClient;

#[async_trait]
impl ClientHandle for NoopClient {
    async fn handshake(
        &self,
        _offered: u32,
    ) -> Result<voom_worker_protocol::HandshakeResponse, ProtocolError> {
        Err(ProtocolError::InternalServerError)
    }

    async fn dispatch(
        &self,
        _creds: &WorkerCredentials,
        _idempotency_key: &str,
        _request: OperationRequest,
    ) -> Result<DispatchStream, ProtocolError> {
        Err(ProtocolError::InternalServerError)
    }
}
