use std::{
    ffi::{OsStr, OsString},
    io,
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    thread,
    time::Duration,
};

const PROBE_TIMEOUT: Duration = Duration::from_secs(15);
const IDENTITY_POLL_WINDOW: Duration = Duration::from_secs(2);
const IDENTITY_POLL_INTERVAL: Duration = Duration::from_millis(100);
const NVIDIA_DEVICE_ENV: &str = "VOOM_NVIDIA_DEVICE";
const NVIDIA_MAX_SESSIONS_ENV: &str = "VOOM_NVIDIA_MAX_SESSIONS";
const NVIDIA_SMI_BIN_ENV: &str = "VOOM_NVIDIA_SMI_BIN";
const DEFAULT_NVIDIA_SMI_BIN: &str = "nvidia-smi";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NvidiaPreflightConfig {
    pub device_uuid: String,
    pub max_sessions: u32,
    pub nvidia_smi_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NvidiaPreflight {
    pub device_uuid: String,
    pub device_name: String,
    pub driver_version: String,
    pub max_sessions: u32,
    pub decoders: Vec<String>,
    pub decoder_diagnostics: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FfmpegPreflight {
    pub ffmpeg_path: PathBuf,
    pub ffprobe_path: PathBuf,
    pub ffmpeg_version: String,
    pub ffprobe_version: String,
    pub hevc_encoder: String,
    pub svtav1_encoder: String,
    pub libaom_encoder: String,
    pub aac_encoder: String,
    pub opus_encoder: String,
    pub matroska_muxer: String,
    pub mp4_muxer: String,
    pub ogg_muxer: String,
    pub nvidia: Option<NvidiaPreflight>,
}

impl FfmpegPreflight {
    /// Returns true when the named video encoder was detected during preflight.
    #[must_use]
    pub fn has_encoder(&self, encoder: &str) -> bool {
        match encoder {
            "libx265" => !self.hevc_encoder.is_empty(),
            "libsvtav1" => !self.svtav1_encoder.is_empty(),
            "libaom-av1" => !self.libaom_encoder.is_empty(),
            "hevc_nvenc" => self.nvidia.is_some(),
            "aac" => !self.aac_encoder.is_empty(),
            "libopus" => !self.opus_encoder.is_empty(),
            _ => false,
        }
    }

    /// Returns true when the named muxer was detected during preflight.
    #[must_use]
    pub fn has_muxer(&self, muxer: &str) -> bool {
        match muxer {
            "matroska" | "mkv" => !self.matroska_muxer.is_empty(),
            "mp4" => !self.mp4_muxer.is_empty(),
            "ogg" => !self.ogg_muxer.is_empty(),
            _ => false,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum FFmpegPreflightError {
    #[error("ffmpeg preflight failed: {0}")]
    Failed(String),
}

pub const FFMPEG_BIN_ENV: &str = "VOOM_FFMPEG_BIN";
pub const FFPROBE_BIN_ENV: &str = "VOOM_FFPROBE_BIN";
const DEFAULT_FFMPEG_BIN: &str = "ffmpeg";
const DEFAULT_FFPROBE_BIN: &str = "ffprobe";

pub fn preflight_from_process_env() -> Result<FfmpegPreflight, FFmpegPreflightError> {
    let ffmpeg =
        std::env::var_os(FFMPEG_BIN_ENV).unwrap_or_else(|| OsString::from(DEFAULT_FFMPEG_BIN));
    let ffprobe =
        std::env::var_os(FFPROBE_BIN_ENV).unwrap_or_else(|| OsString::from(DEFAULT_FFPROBE_BIN));
    let ffmpeg_path = resolve_binary(&ffmpeg);
    let ffprobe_path = resolve_binary(&ffprobe);
    let nvidia = nvidia_config_from_process_env()?;
    match nvidia {
        Some(config) => preflight_with_nvidia(&ffmpeg_path, &ffprobe_path, &config),
        None => preflight_with_paths(&ffmpeg_path, &ffprobe_path),
    }
}

fn nvidia_config_from_process_env() -> Result<Option<NvidiaPreflightConfig>, FFmpegPreflightError> {
    let device = std::env::var(NVIDIA_DEVICE_ENV).ok();
    let sessions = std::env::var(NVIDIA_MAX_SESSIONS_ENV).ok();
    let Some(device_uuid) = device else {
        if sessions.is_some() {
            return Err(FFmpegPreflightError::Failed(format!(
                "{NVIDIA_MAX_SESSIONS_ENV} requires {NVIDIA_DEVICE_ENV}"
            )));
        }
        return Ok(None);
    };
    validate_nvidia_uuid(&device_uuid)?;
    let max_sessions = sessions
        .as_deref()
        .unwrap_or("1")
        .parse::<u32>()
        .map_err(|error| {
            FFmpegPreflightError::Failed(format!(
                "{NVIDIA_MAX_SESSIONS_ENV} must be an integer in 1..=16: {error}"
            ))
        })?;
    if !(1..=16).contains(&max_sessions) {
        return Err(FFmpegPreflightError::Failed(format!(
            "{NVIDIA_MAX_SESSIONS_ENV} must be in 1..=16"
        )));
    }
    let nvidia_smi = std::env::var_os(NVIDIA_SMI_BIN_ENV)
        .unwrap_or_else(|| OsString::from(DEFAULT_NVIDIA_SMI_BIN));
    Ok(Some(NvidiaPreflightConfig {
        device_uuid,
        max_sessions,
        nvidia_smi_path: resolve_binary(&nvidia_smi),
    }))
}

pub fn preflight_with_paths(
    ffmpeg_path: &Path,
    ffprobe_path: &Path,
) -> Result<FfmpegPreflight, FFmpegPreflightError> {
    require_executable_file("ffmpeg", ffmpeg_path)?;
    require_executable_file("ffprobe", ffprobe_path)?;

    let ffmpeg_version = first_output_line(
        "ffmpeg -hide_banner -version",
        command_output(
            Command::new(ffmpeg_path)
                .arg("-hide_banner")
                .arg("-version"),
        ),
    )?;
    let ffprobe_version = first_output_line(
        "ffprobe -hide_banner -version",
        command_output(
            Command::new(ffprobe_path)
                .arg("-hide_banner")
                .arg("-version"),
        ),
    )?;
    let encoders = command_text(
        "ffmpeg -hide_banner -encoders",
        command_output(
            Command::new(ffmpeg_path)
                .arg("-hide_banner")
                .arg("-encoders"),
        ),
    )?;
    let hevc_encoder = parse_token(&encoders, "libx265").ok_or_else(|| {
        FFmpegPreflightError::Failed(
            "ffmpeg does not advertise required libx265 encoder".to_owned(),
        )
    })?;
    let svtav1_encoder = parse_token(&encoders, "libsvtav1").ok_or_else(|| {
        FFmpegPreflightError::Failed(
            "ffmpeg does not advertise required libsvtav1 encoder".to_owned(),
        )
    })?;
    let libaom_encoder = parse_token(&encoders, "libaom-av1").unwrap_or_default();
    let aac_encoder = parse_token(&encoders, "aac").ok_or_else(|| {
        FFmpegPreflightError::Failed("ffmpeg does not advertise required aac encoder".to_owned())
    })?;
    let opus_encoder = parse_token(&encoders, "libopus").ok_or_else(|| {
        FFmpegPreflightError::Failed(
            "ffmpeg does not advertise required libopus encoder".to_owned(),
        )
    })?;
    let muxers = command_text(
        "ffmpeg -hide_banner -muxers",
        command_output(Command::new(ffmpeg_path).arg("-hide_banner").arg("-muxers")),
    )?;
    let matroska_muxer = parse_token(&muxers, "matroska").ok_or_else(|| {
        FFmpegPreflightError::Failed("ffmpeg does not advertise required matroska muxer".to_owned())
    })?;
    let mp4_muxer = parse_token(&muxers, "mp4").ok_or_else(|| {
        FFmpegPreflightError::Failed("ffmpeg does not advertise required mp4 muxer".to_owned())
    })?;
    let ogg_muxer = parse_token(&muxers, "ogg").ok_or_else(|| {
        FFmpegPreflightError::Failed("ffmpeg does not advertise required ogg muxer".to_owned())
    })?;

    Ok(FfmpegPreflight {
        ffmpeg_path: ffmpeg_path.to_owned(),
        ffprobe_path: ffprobe_path.to_owned(),
        ffmpeg_version,
        ffprobe_version,
        hevc_encoder,
        svtav1_encoder,
        libaom_encoder,
        aac_encoder,
        opus_encoder,
        matroska_muxer,
        mp4_muxer,
        ogg_muxer,
        nvidia: None,
    })
}

pub fn preflight_with_nvidia(
    ffmpeg_path: &Path,
    ffprobe_path: &Path,
    config: &NvidiaPreflightConfig,
) -> Result<FfmpegPreflight, FFmpegPreflightError> {
    validate_nvidia_uuid(&config.device_uuid)?;
    if !(1..=16).contains(&config.max_sessions) {
        return Err(FFmpegPreflightError::Failed(
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

fn validate_nvidia_uuid(device_uuid: &str) -> Result<(), FFmpegPreflightError> {
    let Some(uuid) = device_uuid.strip_prefix("GPU-") else {
        return Err(FFmpegPreflightError::Failed(
            "NVIDIA device must be a full GPU- UUID".to_owned(),
        ));
    };
    let valid = uuid.len() == 36
        && uuid.char_indices().all(|(index, ch)| match index {
            8 | 13 | 18 | 23 => ch == '-',
            _ => ch.is_ascii_hexdigit(),
        });
    if !valid {
        return Err(FFmpegPreflightError::Failed(
            "NVIDIA device must be a full GPU- UUID".to_owned(),
        ));
    }
    Ok(())
}

fn probe_nvidia_identity(
    config: &NvidiaPreflightConfig,
) -> Result<(String, String), FFmpegPreflightError> {
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
        return Err(FFmpegPreflightError::Failed(format!(
            "nvidia-smi returned unexpected identity `{line}` for {}",
            config.device_uuid
        )));
    }
    Ok((fields[1].to_owned(), fields[2].to_owned()))
}

fn require_nvidia_build_features(ffmpeg_path: &Path) -> Result<(), FFmpegPreflightError> {
    for (flag, required) in [
        ("-encoders", &["hevc_nvenc"][..]),
        ("-decoders", &["h264_cuvid", "hevc_cuvid", "av1_cuvid"][..]),
        ("-filters", &["hwupload_cuda", "scale_cuda"][..]),
    ] {
        let text = command_text(
            &format!("ffmpeg {flag}"),
            command_output(Command::new(ffmpeg_path).arg("-hide_banner").arg(flag)),
        )?;
        for token in required {
            if parse_token(&text, token).is_none() {
                return Err(FFmpegPreflightError::Failed(format!(
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
) -> Result<(), FFmpegPreflightError> {
    for _attempt in 0..3 {
        if run_identity_attempt(ffmpeg_path, config)? {
            return Ok(());
        }
    }
    Err(FFmpegPreflightError::Failed(
        "NVENC identity probe exited before its PID could be observed after three attempts"
            .to_owned(),
    ))
}

fn run_identity_attempt(
    ffmpeg_path: &Path,
    config: &NvidiaPreflightConfig,
) -> Result<bool, FFmpegPreflightError> {
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
        FFmpegPreflightError::Failed(format!("NVENC identity encode failed to start: {error}"))
    })?;
    let pid = child.id();
    let started = std::time::Instant::now();
    while started.elapsed() < IDENTITY_POLL_WINDOW {
        if let Some(status) = child.try_wait().map_err(|error| {
            FFmpegPreflightError::Failed(format!("polling NVENC identity encode: {error}"))
        })? {
            let output = child.wait_with_output().map_err(|error| {
                FFmpegPreflightError::Failed(format!("reaping NVENC identity encode: {error}"))
            })?;
            if status.success() {
                return Ok(false);
            }
            return Err(FFmpegPreflightError::Failed(format!(
                "NVENC identity encode exited {status}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        if let Some(uuid) = query_compute_uuid(config, pid)? {
            if uuid == config.device_uuid {
                let output = wait_child_output(child, PROBE_TIMEOUT, "NVENC identity encode")?;
                command_text("NVENC identity encode", Ok(output))?;
                return Ok(true);
            }
            kill_and_reap(&mut child);
            return Err(FFmpegPreflightError::Failed(format!(
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
) -> Result<Option<String>, FFmpegPreflightError> {
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
) -> Result<(), FFmpegPreflightError> {
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
) -> Result<(Vec<String>, Vec<String>), FFmpegPreflightError> {
    let probe_dir = DecoderProbeDir::new()?;
    let fixtures = [
        ("h264_cuvid", "libx264", probe_dir.path.join("h264.mkv")),
        ("hevc_cuvid", "libx265", probe_dir.path.join("hevc.mkv")),
        ("av1_cuvid", "libsvtav1", probe_dir.path.join("av1.mkv")),
    ];
    let mut decoders = Vec::new();
    let mut diagnostics = Vec::new();
    for (decoder, encoder, fixture) in &fixtures {
        create_decoder_fixture(ffmpeg_path, encoder, fixture)?;
        match run_decoder_smoke(ffmpeg_path, config, decoder, fixture) {
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
) -> Result<(), FFmpegPreflightError> {
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
) -> Result<(), FFmpegPreflightError> {
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
) -> Result<(), FFmpegPreflightError> {
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
        children.push(command.spawn().map_err(|error| {
            FFmpegPreflightError::Failed(format!(
                "NVENC concurrency probe failed to start: {error}"
            ))
        })?);
    }
    for child in children {
        let output = wait_child_output(child, PROBE_TIMEOUT, "NVENC concurrency probe")?;
        command_text("NVENC concurrency probe", Ok(output))?;
    }
    Ok(())
}

fn nvidia_ffmpeg_command(ffmpeg_path: &Path, config: &NvidiaPreflightConfig) -> Command {
    let mut command = Command::new(ffmpeg_path);
    command.env("CUDA_VISIBLE_DEVICES", &config.device_uuid);
    command
}

struct DecoderProbeDir {
    path: PathBuf,
}

impl DecoderProbeDir {
    fn new() -> Result<Self, FFmpegPreflightError> {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| {
                FFmpegPreflightError::Failed(format!(
                    "system clock before Unix epoch during NVIDIA preflight: {error}"
                ))
            })?
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "voom-nvidia-decoder-probe-{}-{nonce}",
            std::process::id(),
        ));
        std::fs::create_dir(&path).map_err(|error| {
            FFmpegPreflightError::Failed(format!(
                "create NVIDIA decoder probe directory {}: {error}",
                path.display()
            ))
        })?;
        Ok(Self { path })
    }
}

impl Drop for DecoderProbeDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn resolve_binary(binary: &OsStr) -> PathBuf {
    let path = PathBuf::from(binary);
    if path.components().count() > 1 {
        return path;
    }
    let Some(paths) = std::env::var_os("PATH") else {
        return path;
    };
    for dir in std::env::split_paths(&paths) {
        let candidate = dir.join(&path);
        if is_executable_file(&candidate) {
            return candidate;
        }
    }
    path
}

fn require_executable_file(label: &str, path: &Path) -> Result<(), FFmpegPreflightError> {
    if !is_executable_file(path) {
        return Err(FFmpegPreflightError::Failed(format!(
            "{label} binary is missing or not executable: {}",
            path.display()
        )));
    }
    Ok(())
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    is_executable_metadata(&metadata)
}

#[cfg(unix)]
fn is_executable_metadata(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;

    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable_metadata(_metadata: &std::fs::Metadata) -> bool {
    true
}

fn first_output_line(
    command_name: &str,
    output: std::io::Result<std::process::Output>,
) -> Result<String, FFmpegPreflightError> {
    command_text(command_name, output)?
        .lines()
        .next()
        .filter(|line| !line.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| FFmpegPreflightError::Failed(format!("{command_name} produced no output")))
}

fn command_text(
    command_name: &str,
    output: std::io::Result<std::process::Output>,
) -> Result<String, FFmpegPreflightError> {
    let output = output.map_err(|err| {
        FFmpegPreflightError::Failed(format!("{command_name} failed to start: {err}"))
    })?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let text = format!("{stdout}{stderr}");
    if output.status.success() {
        Ok(text)
    } else {
        Err(FFmpegPreflightError::Failed(format!(
            "{command_name} exited with status {}: {}",
            output
                .status
                .code()
                .map_or_else(|| "signal".to_owned(), |code| code.to_string()),
            text.trim()
        )))
    }
}

fn command_output(command: &mut Command) -> io::Result<Output> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut attempts_remaining = 3;
    loop {
        attempts_remaining -= 1;
        match command
            .spawn()
            .and_then(|child| wait_child_output_io(child, PROBE_TIMEOUT, "dependency probe"))
        {
            Err(err) if is_text_file_busy(&err) && attempts_remaining > 0 => {
                thread::sleep(Duration::from_millis(10));
            }
            result => return result,
        }
    }
}

fn wait_child_output(
    child: Child,
    deadline: Duration,
    label: &str,
) -> Result<Output, FFmpegPreflightError> {
    wait_child_output_io(child, deadline, label).map_err(|error| {
        FFmpegPreflightError::Failed(format!("{label} failed while waiting: {error}"))
    })
}

fn wait_child_output_io(mut child: Child, deadline: Duration, label: &str) -> io::Result<Output> {
    let started = std::time::Instant::now();
    loop {
        if child.try_wait()?.is_some() {
            return child.wait_with_output();
        }
        if started.elapsed() >= deadline {
            kill_and_reap(&mut child);
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("{label} exceeded {} seconds", deadline.as_secs()),
            ));
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn kill_and_reap(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn is_text_file_busy(err: &io::Error) -> bool {
    err.raw_os_error() == Some(26)
}

fn parse_token(text: &str, token: &str) -> Option<String> {
    text.lines()
        .find(|line| line.split_whitespace().any(|candidate| candidate == token))
        .map(|_| token.to_owned())
}

#[cfg(test)]
#[path = "preflight_test.rs"]
mod tests;
