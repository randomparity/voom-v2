#![expect(
    clippy::print_stdout,
    reason = "ffmpeg-worker advertises readiness with BOUND addr=..."
)]

use std::net::SocketAddr;

use secrecy::SecretString;
use voom_ffmpeg_worker::{
    ALL_VIDEO_ENCODERS, DEFAULT_PROCESS_TIMEOUT, FfmpegConfig, operation_handler,
    preflight_from_process_env,
};
use voom_worker_protocol::{HttpServer, ServerHandle, WorkerCredentials};

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let credentials = load_credentials()?;
    let preflight = preflight_from_process_env()?;
    let available_video_encoders: Vec<String> = ALL_VIDEO_ENCODERS
        .iter()
        .filter(|encoder| preflight.has_encoder(encoder))
        .map(|encoder| (*encoder).to_owned())
        .collect();
    let config = FfmpegConfig::new(
        preflight.ffmpeg_path,
        preflight.ffprobe_path,
        preflight.ffmpeg_version,
        DEFAULT_PROCESS_TIMEOUT,
    )
    .with_available_video_encoders(available_video_encoders);
    let bind: SocketAddr = std::env::var("VOOM_WORKER_BIND")
        .unwrap_or_else(|_| "127.0.0.1:0".to_owned())
        .parse()
        .map_err(|err| format!("VOOM_WORKER_BIND parse failed: {err}"))?;

    let server = HttpServer::new(credentials, operation_handler(config));
    let running = server
        .serve(bind)
        .await
        .map_err(|err| format!("serve failed: {err}"))?;

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

fn load_credentials() -> Result<WorkerCredentials, Box<dyn std::error::Error>> {
    let secret = std::env::var("VOOM_WORKER_SECRET").map_err(|_| "VOOM_WORKER_SECRET not set")?;
    let worker_id: u64 = std::env::var("VOOM_WORKER_ID")
        .map_err(|_| "VOOM_WORKER_ID not set")?
        .parse()
        .map_err(|_| "VOOM_WORKER_ID not parseable")?;
    let worker_epoch: u64 = std::env::var("VOOM_WORKER_EPOCH")
        .map_err(|_| "VOOM_WORKER_EPOCH not set")?
        .parse()
        .map_err(|_| "VOOM_WORKER_EPOCH not parseable")?;
    Ok(WorkerCredentials {
        worker_id: voom_core::WorkerId(worker_id),
        worker_epoch,
        secret: SecretString::from(secret),
    })
}
