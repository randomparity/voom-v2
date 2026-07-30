#![expect(
    clippy::print_stdout,
    reason = "ffmpeg-worker advertises readiness with BOUND addr=..."
)]
#![cfg_attr(
    test,
    expect(
        clippy::unwrap_used,
        reason = "tests use direct unwraps for assertion plumbing"
    )
)]

use voom_ffmpeg_worker::{
    ALL_VIDEO_ENCODERS, AcceleratorBinding, DEFAULT_PROCESS_TIMEOUT, FfmpegConfig,
    VaapiDeviceBinding, operation_handler, preflight, preflight_from_process_env,
};
use voom_worker_protocol::{
    HttpServer, LocalWorkerBound, NvidiaVideoAcceleratorDescriptor,
    VaapiVideoAcceleratorDescriptor, VideoAcceleratorDescriptor,
    VideoToolboxVideoAcceleratorDescriptor, WorkerStartupError, load_worker_bind_addr_from_env,
    load_worker_credentials_from_env, serve_worker_http,
};

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<(), WorkerStartupError> {
    let credentials = load_worker_credentials_from_env()?;
    let preflight = preflight_from_process_env().map_err(WorkerStartupError::dependency)?;
    let binding = bound_accelerator(&preflight)?;
    let accelerator = binding.as_ref().map(advertised_accelerator);
    let config = ffmpeg_config_from_preflight(preflight, binding);
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

fn videotoolbox_descriptor(
    videotoolbox: preflight::VideoToolboxPreflight,
) -> VideoToolboxVideoAcceleratorDescriptor {
    VideoToolboxVideoAcceleratorDescriptor {
        hardware_token: format!("videotoolbox:{}", videotoolbox.resource_id),
        resource_id: videotoolbox.resource_id,
        model_identifier: videotoolbox.model_identifier,
        chip_name: videotoolbox.chip_name,
        macos_version: videotoolbox.macos_version,
        macos_build: videotoolbox.macos_build,
        encoders: videotoolbox.encoders,
        decoders: videotoolbox.decoders,
        max_sessions: videotoolbox.max_sessions,
    }
}

fn nvidia_descriptor(nvidia: preflight::NvidiaPreflight) -> NvidiaVideoAcceleratorDescriptor {
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

/// The one device this worker bound, if any.
///
/// One worker binds one accelerator, so both backends being configured is a
/// configuration error rather than something to silently pick between. Preflight
/// already refuses it; deciding it once here means neither the advertised
/// descriptor nor the command builder can later disagree about which device is
/// bound.
fn bound_accelerator(
    preflight: &preflight::FfmpegPreflight,
) -> Result<Option<AcceleratorBinding>, WorkerStartupError> {
    match (&preflight.nvidia, &preflight.vaapi, &preflight.videotoolbox) {
        (None, None, None) => Ok(None),
        (Some(nvidia), None, None) => Ok(Some(AcceleratorBinding::Nvidia(nvidia_descriptor(
            nvidia.clone(),
        )))),
        (None, Some(vaapi), None) => Ok(Some(AcceleratorBinding::Vaapi(vaapi_device_binding(
            vaapi.clone(),
        )))),
        (None, None, Some(videotoolbox)) => Ok(Some(AcceleratorBinding::VideoToolbox(
            videotoolbox_descriptor(videotoolbox.clone()),
        ))),
        _ => Err(WorkerStartupError::dependency(
            "worker bound more than one accelerator; run one worker per device".to_owned(),
        )),
    }
}

/// Tags the bound device for the `BOUND` readiness line. The render node stays out
/// of the advertised payload: the control plane schedules on the PCI address, and a
/// node path is a local detail of the worker that resolved it.
fn advertised_accelerator(binding: &AcceleratorBinding) -> VideoAcceleratorDescriptor {
    match binding {
        AcceleratorBinding::Nvidia(nvidia) => VideoAcceleratorDescriptor::Nvidia(nvidia.clone()),
        AcceleratorBinding::Vaapi(vaapi) => {
            VideoAcceleratorDescriptor::Vaapi(vaapi.descriptor.clone())
        }
        AcceleratorBinding::VideoToolbox(videotoolbox) => {
            VideoAcceleratorDescriptor::VideoToolbox(videotoolbox.clone())
        }
    }
}

/// Pairs the probe-proven capability with the render node the probes ran on, so
/// command generation can name that node for `-vaapi_device` / `-hwaccel_device`
/// without re-resolving the PCI address.
fn vaapi_device_binding(vaapi: preflight::VaapiPreflight) -> VaapiDeviceBinding {
    VaapiDeviceBinding {
        render_node: vaapi.render_node.clone(),
        descriptor: vaapi_accelerator_descriptor(vaapi),
    }
}

/// The VAAPI capability the worker advertises.
///
/// Every field comes from a probe that ran on the bound node (ADR 0052 §2), so
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
    binding: Option<AcceleratorBinding>,
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
    match binding {
        Some(AcceleratorBinding::Nvidia(nvidia)) => config.with_accelerator(nvidia),
        Some(AcceleratorBinding::Vaapi(vaapi)) => config.with_vaapi_device(vaapi),
        Some(AcceleratorBinding::VideoToolbox(videotoolbox)) => {
            config.with_videotoolbox_device(videotoolbox)
        }
        None => config,
    }
}

#[cfg(test)]
#[path = "main_test.rs"]
mod tests;
