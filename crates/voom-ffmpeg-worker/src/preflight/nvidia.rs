use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::Duration,
};

use voom_core::NVIDIA_VIDEO_DECODERS;

use super::{
    FfmpegPreflight, FfmpegPreflightError, preflight_with_paths,
    process::{
        PROBE_TIMEOUT, ProbeDir, command_output, command_text, kill_and_reap, kill_and_reap_all,
        parse_token, require_executable_file, wait_child_output,
    },
};

const IDENTITY_POLL_WINDOW: Duration = Duration::from_secs(2);
const IDENTITY_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// NVIDIA device binding and declared concurrent encode capacity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NvidiaPreflightConfig {
    pub device_uuid: String,
    pub max_sessions: u32,
    pub nvidia_smi_path: PathBuf,
}

/// NVIDIA identity and capabilities proven by startup probes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NvidiaPreflight {
    pub device_uuid: String,
    pub device_name: String,
    pub driver_version: String,
    pub max_sessions: u32,
    pub decoders: Vec<String>,
    pub decoder_diagnostics: Vec<String>,
}

/// Binds `FFmpeg` probes to one NVIDIA UUID and proves identity and capacity.
///
/// # Errors
///
/// Returns an error when configuration, binaries, identity, features, codec probes, or declared
/// concurrency cannot be proven.
pub fn preflight_with_nvidia(
    ffmpeg_path: &Path,
    ffprobe_path: &Path,
    config: &NvidiaPreflightConfig,
) -> Result<FfmpegPreflight, FfmpegPreflightError> {
    validate_nvidia_uuid(&config.device_uuid)?;
    if !(1..=16).contains(&config.max_sessions) {
        return Err(FfmpegPreflightError::Failed(
            "NVIDIA max sessions must be in 1..=16".to_owned(),
        ));
    }
    require_executable_file("nvidia-smi", &config.nvidia_smi_path)?;
    let mut preflight = preflight_with_paths(ffmpeg_path, ffprobe_path)?;
    let (device_name, driver_version) = probe_nvidia_identity(config)?;
    require_nvidia_build_features(ffmpeg_path)?;
    prove_nvidia_process_identity(ffmpeg_path, config)?;
    run_hevc_nvenc_smoke(ffmpeg_path, config)?;
    let (decoders, decoder_diagnostics) = probe_nvidia_decoders(ffmpeg_path, config)?;
    prove_nvidia_capacity(ffmpeg_path, config)?;
    preflight.nvidia = Some(NvidiaPreflight {
        device_uuid: config.device_uuid.clone(),
        device_name,
        driver_version,
        max_sessions: config.max_sessions,
        decoders,
        decoder_diagnostics,
    });
    Ok(preflight)
}

pub(super) fn validate_nvidia_uuid(device_uuid: &str) -> Result<(), FfmpegPreflightError> {
    let Some(uuid) = device_uuid.strip_prefix("GPU-") else {
        return Err(FfmpegPreflightError::Failed(
            "NVIDIA device must be a full GPU- UUID".to_owned(),
        ));
    };
    let valid = uuid.len() == 36
        && uuid.char_indices().all(|(index, ch)| match index {
            8 | 13 | 18 | 23 => ch == '-',
            _ => ch.is_ascii_hexdigit(),
        });
    if !valid {
        return Err(FfmpegPreflightError::Failed(
            "NVIDIA device must be a full GPU- UUID".to_owned(),
        ));
    }
    Ok(())
}

fn probe_nvidia_identity(
    config: &NvidiaPreflightConfig,
) -> Result<(String, String), FfmpegPreflightError> {
    let output = command_text(
        "nvidia-smi device identity",
        command_output(
            Command::new(&config.nvidia_smi_path)
                .arg("-i")
                .arg(&config.device_uuid)
                .arg("--query-gpu=uuid,name,driver_version")
                .arg("--format=csv,noheader,nounits"),
        ),
    )?;
    let line = output.lines().next().unwrap_or_default();
    let fields: Vec<&str> = line.split(',').map(str::trim).collect();
    if fields.len() != 3 || fields[0] != config.device_uuid {
        return Err(FfmpegPreflightError::Failed(format!(
            "nvidia-smi returned unexpected identity `{line}` for {}",
            config.device_uuid
        )));
    }
    Ok((fields[1].to_owned(), fields[2].to_owned()))
}

fn require_nvidia_build_features(ffmpeg_path: &Path) -> Result<(), FfmpegPreflightError> {
    for (flag, required) in [
        ("-encoders", &["hevc_nvenc"][..]),
        ("-filters", &["hwupload_cuda", "scale_cuda"][..]),
    ] {
        let text = command_text(
            &format!("ffmpeg {flag}"),
            command_output(Command::new(ffmpeg_path).arg("-hide_banner").arg(flag)),
        )?;
        for token in required {
            if parse_token(&text, token).is_none() {
                return Err(FfmpegPreflightError::Failed(format!(
                    "ffmpeg does not advertise required NVIDIA feature `{token}`"
                )));
            }
        }
    }
    Ok(())
}

fn prove_nvidia_process_identity(
    ffmpeg_path: &Path,
    config: &NvidiaPreflightConfig,
) -> Result<(), FfmpegPreflightError> {
    for _attempt in 0..3 {
        if run_identity_attempt(ffmpeg_path, config)? {
            return Ok(());
        }
    }
    Err(FfmpegPreflightError::Failed(
        "NVENC identity probe exited before its PID could be observed after three attempts"
            .to_owned(),
    ))
}

fn run_identity_attempt(
    ffmpeg_path: &Path,
    config: &NvidiaPreflightConfig,
) -> Result<bool, FfmpegPreflightError> {
    let mut command = nvidia_ffmpeg_command(ffmpeg_path, config);
    command.args([
        "-hide_banner",
        "-nostdin",
        "-f",
        "lavfi",
        "-re",
        "-i",
        "testsrc2=size=256x256:rate=30",
        "-t",
        "3",
        "-an",
        "-c:v",
        "hevc_nvenc",
        "-rc",
        "vbr",
        "-cq",
        "23",
        "-b:v",
        "0",
        "-preset",
        "p4",
        "-f",
        "null",
        "-",
    ]);
    command.stdout(Stdio::null()).stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|error| {
        FfmpegPreflightError::Failed(format!("NVENC identity encode failed to start: {error}"))
    })?;
    let pid = child.id();
    let started = std::time::Instant::now();
    while started.elapsed() < IDENTITY_POLL_WINDOW {
        if let Some(status) = child.try_wait().map_err(|error| {
            FfmpegPreflightError::Failed(format!("polling NVENC identity encode: {error}"))
        })? {
            let output = child.wait_with_output().map_err(|error| {
                FfmpegPreflightError::Failed(format!("reaping NVENC identity encode: {error}"))
            })?;
            if status.success() {
                return Ok(false);
            }
            return Err(FfmpegPreflightError::Failed(format!(
                "NVENC identity encode exited {status}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        let uuid = match query_compute_uuid(config, pid) {
            Ok(uuid) => uuid,
            Err(error) => {
                kill_and_reap(&mut child);
                return Err(error);
            }
        };
        if let Some(uuid) = uuid {
            if uuid == config.device_uuid {
                let output = wait_child_output(child, PROBE_TIMEOUT, "NVENC identity encode")?;
                command_text("NVENC identity encode", Ok(output))?;
                return Ok(true);
            }
            kill_and_reap(&mut child);
            return Err(FfmpegPreflightError::Failed(format!(
                "NVENC PID {pid} ran on `{uuid}`, expected `{}`",
                config.device_uuid
            )));
        }
        thread::sleep(IDENTITY_POLL_INTERVAL);
    }
    kill_and_reap(&mut child);
    Ok(false)
}

fn query_compute_uuid(
    config: &NvidiaPreflightConfig,
    pid: u32,
) -> Result<Option<String>, FfmpegPreflightError> {
    let output = command_text(
        "nvidia-smi compute-app identity",
        command_output(
            Command::new(&config.nvidia_smi_path)
                .arg("--query-compute-apps=pid,gpu_uuid")
                .arg("--format=csv,noheader,nounits"),
        ),
    )?;
    for line in output.lines() {
        let fields: Vec<&str> = line.split(',').map(str::trim).collect();
        if fields.len() == 2 && fields[0] == pid.to_string() {
            return Ok(Some(fields[1].to_owned()));
        }
    }
    Ok(None)
}

fn run_hevc_nvenc_smoke(
    ffmpeg_path: &Path,
    config: &NvidiaPreflightConfig,
) -> Result<(), FfmpegPreflightError> {
    let mut command = nvidia_ffmpeg_command(ffmpeg_path, config);
    command.args([
        "-hide_banner",
        "-nostdin",
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=256x256:rate=1",
        "-frames:v",
        "1",
        "-an",
        "-c:v",
        "hevc_nvenc",
        "-rc",
        "vbr",
        "-cq",
        "23",
        "-b:v",
        "0",
        "-preset",
        "p4",
        "-f",
        "null",
        "-",
    ]);
    command_text("HEVC NVENC smoke encode", command_output(&mut command)).map(|_| ())
}

fn probe_nvidia_decoders(
    ffmpeg_path: &Path,
    config: &NvidiaPreflightConfig,
) -> Result<(Vec<String>, Vec<String>), FfmpegPreflightError> {
    let probe_dir = ProbeDir::new("nvidia-decoder-probe")?;
    let mut decoders = Vec::new();
    let mut diagnostics = Vec::new();
    for (codec, decoder) in NVIDIA_VIDEO_DECODERS {
        let encoder = if *codec == "h264" {
            "libx264"
        } else if *codec == "hevc" {
            "libx265"
        } else if *codec == "av1" {
            "libsvtav1"
        } else {
            return Err(FfmpegPreflightError::Failed(format!(
                "NVIDIA decoder probe has no fixture encoder for `{codec}`"
            )));
        };
        let fixture = probe_dir.path().join(format!("{codec}.mkv"));
        create_decoder_fixture(ffmpeg_path, encoder, &fixture)?;
        match run_decoder_smoke(ffmpeg_path, config, decoder, &fixture) {
            Ok(()) => decoders.push((*decoder).to_owned()),
            Err(error) => diagnostics.push(format!("{decoder}: {error}")),
        }
    }
    Ok((decoders, diagnostics))
}

fn create_decoder_fixture(
    ffmpeg_path: &Path,
    encoder: &str,
    output: &Path,
) -> Result<(), FfmpegPreflightError> {
    let mut command = Command::new(ffmpeg_path);
    command.args([
        "-hide_banner",
        "-nostdin",
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=256x256:rate=1",
        "-frames:v",
        "1",
        "-an",
        "-c:v",
        encoder,
        "-y",
    ]);
    command.arg(output);
    command_text(
        &format!("create {encoder} decoder fixture"),
        command_output(&mut command),
    )
    .map(|_| ())
}

fn run_decoder_smoke(
    ffmpeg_path: &Path,
    config: &NvidiaPreflightConfig,
    decoder: &str,
    fixture: &Path,
) -> Result<(), FfmpegPreflightError> {
    let mut command = nvidia_ffmpeg_command(ffmpeg_path, config);
    command.args([
        "-hide_banner",
        "-nostdin",
        "-hwaccel",
        "cuda",
        "-hwaccel_device",
        "0",
        "-hwaccel_output_format",
        "cuda",
        "-c:v",
        decoder,
        "-i",
    ]);
    command.arg(fixture).args([
        "-vf",
        "scale_cuda=w=256:h=256:format=nv12",
        "-frames:v",
        "1",
        "-f",
        "null",
        "-",
    ]);
    command_text(
        &format!("{decoder} exact-device smoke decode"),
        command_output(&mut command),
    )
    .map(|_| ())
}

fn prove_nvidia_capacity(
    ffmpeg_path: &Path,
    config: &NvidiaPreflightConfig,
) -> Result<(), FfmpegPreflightError> {
    let mut children = Vec::new();
    for _session in 0..config.max_sessions {
        let mut command = nvidia_ffmpeg_command(ffmpeg_path, config);
        command.args([
            "-hide_banner",
            "-nostdin",
            "-f",
            "lavfi",
            "-re",
            "-i",
            "testsrc2=size=256x256:rate=30",
            "-t",
            "1",
            "-an",
            "-c:v",
            "hevc_nvenc",
            "-rc",
            "vbr",
            "-cq",
            "23",
            "-b:v",
            "0",
            "-preset",
            "p4",
            "-f",
            "null",
            "-",
        ]);
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        match command.spawn() {
            Ok(child) => children.push(child),
            Err(error) => {
                kill_and_reap_all(&mut children);
                return Err(FfmpegPreflightError::Failed(format!(
                    "NVENC concurrency probe failed to start: {error}"
                )));
            }
        }
    }
    while let Some(child) = children.pop() {
        let result = wait_child_output(child, PROBE_TIMEOUT, "NVENC concurrency probe")
            .and_then(|output| command_text("NVENC concurrency probe", Ok(output)));
        if let Err(error) = result {
            kill_and_reap_all(&mut children);
            return Err(error);
        }
    }
    Ok(())
}

fn nvidia_ffmpeg_command(ffmpeg_path: &Path, config: &NvidiaPreflightConfig) -> Command {
    let mut command = Command::new(ffmpeg_path);
    command.env("CUDA_VISIBLE_DEVICES", &config.device_uuid);
    command
}

#[cfg(test)]
#[path = "nvidia_test.rs"]
mod tests;
