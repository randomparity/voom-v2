#![expect(
    clippy::print_stdout,
    reason = "verify-artifact-worker advertises readiness with BOUND addr=..."
)]

use voom_verify_artifact_worker::operation_handler;
use voom_worker_protocol::{
    HttpServer, WorkerCredentials, WorkerStartupError, load_worker_bind_addr_from_env,
    load_worker_credentials_from_env, serve_worker_http,
};

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<(), WorkerStartupError> {
    let credentials = load_worker_credentials_from_env()?;
    let bind = load_worker_bind_addr_from_env()?;

    let server = worker_server(credentials);
    let running = serve_worker_http(&server, bind).await?;

    println!("BOUND addr={}", running.bound);

    let shutdown_tx = running.shutdown;
    let joined = running.joined;
    let watchdog = std::thread::spawn(move || {
        let mut stdin = std::io::stdin().lock();
        let mut buffer = [0_u8; 1024];
        loop {
            match std::io::Read::read(&mut stdin, &mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
        let _ = shutdown_tx.send(());
    });

    let _ = watchdog.join();
    let _ = joined.await;
    Ok(())
}

fn worker_server(credentials: WorkerCredentials) -> HttpServer {
    HttpServer::new(credentials, operation_handler())
}

#[cfg(test)]
#[path = "main_test.rs"]
mod tests;
