#![expect(
    clippy::print_stdout,
    reason = "ffmpeg-worker advertises readiness with BOUND addr=..."
)]

use voom_ffmpeg_worker::{
    ALL_VIDEO_ENCODERS, DEFAULT_PROCESS_TIMEOUT, FfmpegConfig, operation_handler, preflight,
    preflight_from_process_env,
};
use voom_worker_protocol::{
    HttpServer, LocalWorkerBound, NvidiaVideoAcceleratorDescriptor,
    VaapiVideoAcceleratorDescriptor, VideoAcceleratorDescriptor, WorkerStartupError,
    load_worker_bind_addr_from_env, load_worker_credentials_from_env, serve_worker_http,
};

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<(), WorkerStartupError> {
    let credentials = load_worker_credentials_from_env()?;
    let preflight = preflight_from_process_env().map_err(WorkerStartupError::dependency)?;
    let nvidia = preflight.nvidia.clone().map(accelerator_descriptor);
    let vaapi = preflight.vaapi.clone().map(vaapi_accelerator_descriptor);
    let accelerator = advertised_accelerator(nvidia.clone(), vaapi)?;
    let config = ffmpeg_config_from_preflight(preflight, nvidia);
    let bind = load_worker_bind_addr_from_env()?;

    let server = HttpServer::new(credentials, operation_handler(config));
    let running = serve_worker_http(&server, bind).await?;

    match accelerator {
        Some(accelerator) => {
            let bound = LocalWorkerBound {
                addr: running.bound,
                accelerator: Some(accelerator),
            };
            let bound = serde_json::to_string(&bound).map_err(WorkerStartupError::dependency)?;
            println!("BOUND {bound}");
        }
        None => println!("BOUND addr={}", running.bound),
    }

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

fn accelerator_descriptor(nvidia: preflight::NvidiaPreflight) -> NvidiaVideoAcceleratorDescriptor {
    NvidiaVideoAcceleratorDescriptor {
        hardware_token: format!("nvidia:{}", nvidia.device_uuid),
        device_uuid: nvidia.device_uuid,
        device_name: nvidia.device_name,
        driver_version: nvidia.driver_version,
        encoders: vec!["hevc_nvenc".to_owned()],
        decoders: nvidia.decoders,
        max_sessions: nvidia.max_sessions,
    }
}

/// Tags the one device this worker bound, if any.
///
/// One worker binds one accelerator, so both backends being configured is a
/// configuration error rather than something to silently pick between. Preflight
/// already refuses it; this keeps the invariant visible where the descriptor is
/// built instead of leaving an unreachable arm.
fn advertised_accelerator(
    nvidia: Option<NvidiaVideoAcceleratorDescriptor>,
    vaapi: Option<VaapiVideoAcceleratorDescriptor>,
) -> Result<Option<VideoAcceleratorDescriptor>, WorkerStartupError> {
    match (nvidia, vaapi) {
        (None, None) => Ok(None),
        (Some(nvidia), None) => Ok(Some(VideoAcceleratorDescriptor::Nvidia(nvidia))),
        (None, Some(vaapi)) => Ok(Some(VideoAcceleratorDescriptor::Vaapi(vaapi))),
        (Some(nvidia), Some(vaapi)) => Err(WorkerStartupError::dependency(format!(
            "worker bound both NVIDIA {} and VAAPI PCI {}; run one worker per device",
            nvidia.device_uuid, vaapi.pci_address
        ))),
    }
}

/// The VAAPI capability the worker advertises.
///
/// Every field comes from a probe that ran on the bound node (ADR 0051 §2), so
/// `encoders` and `decoders` are what this driver build actually did, not what
/// `FFmpeg` or `vainfo` listed. There is no `hardware_token` field: the token is
/// derived from the PCI address at the binding site.
fn vaapi_accelerator_descriptor(
    vaapi: preflight::VaapiPreflight,
) -> VaapiVideoAcceleratorDescriptor {
    VaapiVideoAcceleratorDescriptor {
        pci_address: vaapi.pci_address,
        device_name: vaapi.device_name,
        driver_version: vaapi.driver_version,
        encoders: vaapi.encoders,
        decoders: vaapi.decoders,
        max_sessions: vaapi.max_sessions,
    }
}

fn ffmpeg_config_from_preflight(
    preflight: preflight::FfmpegPreflight,
    accelerator: Option<NvidiaVideoAcceleratorDescriptor>,
) -> FfmpegConfig {
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
    match accelerator {
        Some(accelerator) => config.with_accelerator(accelerator),
        None => config,
    }
}

#[cfg(test)]
#[path = "main_test.rs"]
mod tests;
