use std::net::SocketAddr;
use std::sync::Arc;

use time::OffsetDateTime;
use tokio::io::AsyncReadExt;
use voom_worker_protocol::{
    HttpServer, OperationDispatch, OperationFuture, OperationRequest, OperationResponse,
    PercentBps, ProgressFrame, ProtocolError, WorkerStartupError, load_worker_bind_addr_from_env,
    load_worker_credentials_from_env, serve_worker_http,
};

use crate::catalog::{ProviderDefinition, operation_name, provider_definition, provider_entry};
use crate::results::result_payload;
use crate::streaming::{TimedDispatch, body_from_frames, progress_frame};
use crate::validation::{TimingControls, invalid, scenario, validate_payload};

pub fn dispatch_provider(
    provider: &ProviderDefinition,
    req: &OperationRequest,
) -> Result<OperationDispatch, ProtocolError> {
    let entry =
        provider_entry(provider.binary_name).ok_or_else(|| ProtocolError::UnknownOperation {
            name: provider.binary_name.to_owned(),
        })?;
    if !crate::catalog::supports_operation(&entry.definition, req.operation) {
        return Err(ProtocolError::UnknownOperation {
            name: operation_name(req.operation),
        });
    }

    let scenario = scenario(&req.payload);
    validate_payload(entry.kind, req)?;
    let timing = TimingControls::from_payload(&req.payload)?;
    let now = OffsetDateTime::now_utc();
    let response = OperationResponse {
        lease_id: req.lease_id,
        accepted_at: now,
    };
    let result_payload = result_payload(
        provider.provider,
        req.operation,
        scenario,
        &req.payload,
        timing.fan_out_count,
    )?;

    if timing.duration_ms == 0 {
        let progress = progress_frame(
            req.lease_id,
            0,
            now,
            PercentBps::ZERO,
            provider.provider,
            req.operation,
            scenario,
        );
        let result = ProgressFrame::Result {
            lease_id: req.lease_id,
            seq: 1,
            emitted_at: now,
            payload: result_payload,
        };
        return Ok(OperationDispatch::buffered(
            response,
            body_from_frames(&[progress, result])?,
        ));
    }

    let handle = tokio::runtime::Handle::try_current()
        .map_err(|_| invalid("timed fake dispatch requires tokio runtime"))?;
    let (writer, dispatch) = OperationDispatch::streaming(response);
    let timed = TimedDispatch {
        writer,
        lease_id: req.lease_id,
        provider: provider.provider.to_owned(),
        operation: req.operation,
        scenario: scenario.to_owned(),
        result_payload,
        duration_ms: timing.duration_ms,
        progress_interval_ms: timing.progress_interval_ms,
    };
    handle.spawn(async move {
        timed.emit().await;
    });
    Ok(dispatch)
}

pub async fn run_provider(binary_name: &'static str) -> Result<(), WorkerStartupError> {
    let provider = provider_definition(binary_name)
        .ok_or_else(|| WorkerStartupError::unknown_provider(binary_name))?;
    let credentials = load_worker_credentials_from_env()?;
    let bind = load_worker_bind_addr_from_env()?;
    let server = HttpServer::new(
        credentials,
        Arc::new(move |req| {
            let provider = provider;
            Box::pin(async move { dispatch_provider(&provider, &req) }) as OperationFuture
        }),
    );
    let running = serve_worker_http(&server, bind).await?;
    print_bound(running.bound);
    let shutdown_tx = running.shutdown;
    let joined = running.joined;
    let watchdog = tokio::spawn(async move {
        let mut stdin = tokio::io::stdin();
        let mut bytes = Vec::new();
        let _ = stdin.read_to_end(&mut bytes).await;
        let _ = shutdown_tx.send(());
    });
    let _ = watchdog.await;
    let _ = joined.await;
    Ok(())
}

#[expect(
    clippy::print_stdout,
    reason = "fake providers advertise readiness with BOUND addr=..."
)]
fn print_bound(bound: SocketAddr) {
    println!("BOUND addr={bound}");
}
