#![expect(
    clippy::print_stdout,
    reason = "echo-worker advertises its bound address via println!(\"BOUND addr=...\") as part of its contract with the conformance harness"
)]
//! Minimal worker that exists solely to validate the wire contract.
//! Phase 1 design §4.5.
//!
//! Behavior:
//! - Reads `VOOM_WORKER_SECRET` / `VOOM_WORKER_ID` /
//!   `VOOM_WORKER_EPOCH` / `VOOM_WORKER_BIND` from env.
//! - Binds the requested address, prints `BOUND addr=<actual>`.
//! - Implements `ProbeFile` only: emits one `Progress` (seq=0) then
//!   one `Result` (seq=1) echoing the path back.
//! - Self-exits when stdin closes (parent-death watchdog).

use std::sync::Arc;

use time::OffsetDateTime;
use tokio::io::{AsyncBufReadExt, BufReader};
use voom_worker_protocol::http::OperationDispatch;
use voom_worker_protocol::{
    HttpServer, OperationFuture, OperationKind, OperationRequest, OperationResponse, ProgressFrame,
    ProtocolError, WorkerStartupError, load_worker_bind_addr_from_env,
    load_worker_credentials_from_env, serve_worker_http,
};

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<(), WorkerStartupError> {
    let credentials = load_worker_credentials_from_env()?;
    let bind = load_worker_bind_addr_from_env()?;

    let handler = Arc::new(handle_operation);
    let server = HttpServer::new(credentials, handler);
    let running = serve_worker_http(&server, bind).await?;

    println!("BOUND addr={}", running.bound);

    // Parent-death watchdog: read stdin to EOF and then trigger shutdown.
    let shutdown_tx = running.shutdown;
    let joined = running.joined;
    let watchdog = tokio::spawn(async move {
        let mut stdin = BufReader::new(tokio::io::stdin()).lines();
        while let Ok(Some(_)) = stdin.next_line().await {
            // Drain any stdin input; we only care about EOF.
        }
        let _ = shutdown_tx.send(());
    });

    let _ = watchdog.await;
    let _ = joined.await;
    Ok(())
}

pub(crate) fn handle_operation(req: OperationRequest) -> OperationFuture {
    Box::pin(async move {
        if req.operation != OperationKind::ProbeFile {
            return Err(ProtocolError::UnknownOperation {
                name: format!("{:?}", req.operation),
            });
        }
        let path = req
            .payload
            .get("path")
            .and_then(serde_json::Value::as_str)
            .ok_or(ProtocolError::InvalidPayload {
                detail: "payload missing path".to_owned(),
            })?
            .to_owned();
        let now = OffsetDateTime::now_utc();
        let progress = ProgressFrame::Progress {
            lease_id: req.lease_id,
            seq: 0,
            emitted_at: now,
            percent: None,
            message: Some(format!("probing {path}")),
            payload: None,
        };
        let result = ProgressFrame::Result {
            lease_id: req.lease_id,
            seq: 1,
            emitted_at: now,
            payload: serde_json::json!({"echoed_path": path}),
        };
        let mut body: Vec<u8> = Vec::new();
        body.extend_from_slice(&serde_json::to_vec(&progress).map_err(|e| {
            ProtocolError::InvalidPayload {
                detail: e.to_string(),
            }
        })?);
        body.push(b'\n');
        body.extend_from_slice(&serde_json::to_vec(&result).map_err(|e| {
            ProtocolError::InvalidPayload {
                detail: e.to_string(),
            }
        })?);
        body.push(b'\n');
        Ok(OperationDispatch::buffered(
            OperationResponse {
                lease_id: req.lease_id,
                accepted_at: now,
            },
            body,
        ))
    })
}
