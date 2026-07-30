use std::{
    ffi::{OsStr, OsString},
    io::{self, Read},
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    thread,
    time::Duration,
};
use voom_core::{NVIDIA_VIDEO_DECODERS, VAAPI_VIDEO_DECODERS};

const PROBE_TIMEOUT: Duration = Duration::from_secs(15);
const VAAPI_HEVC_ENCODER: &str = "hevc_vaapi";
const IDENTITY_POLL_WINDOW: Duration = Duration::from_secs(2);
const IDENTITY_POLL_INTERVAL: Duration = Duration::from_millis(100);
const NVIDIA_DEVICE_ENV: &str = "VOOM_NVIDIA_DEVICE";
const NVIDIA_MAX_SESSIONS_ENV: &str = "VOOM_NVIDIA_MAX_SESSIONS";
const NVIDIA_SMI_BIN_ENV: &str = "VOOM_NVIDIA_SMI_BIN";
const DEFAULT_NVIDIA_SMI_BIN: &str = "nvidia-smi";
pub const VAAPI_DEVICE_ENV: &str = "VOOM_VAAPI_DEVICE";
pub const VAAPI_MAX_SESSIONS_ENV: &str = "VOOM_VAAPI_MAX_SESSIONS";
pub const DRI_ROOT_ENV: &str = "VOOM_DRI_ROOT";
pub const DRM_SYSFS_ROOT_ENV: &str = "VOOM_DRM_SYSFS_ROOT";
const DEFAULT_DRI_ROOT: &str = "/dev/dri";
const DEFAULT_DRM_SYSFS_ROOT: &str = "/sys/class/drm";

/// The three clocks ADR 0051 §7 adopts unchanged from ADR 0049 §3.
///
/// They are constants rather than operator configuration: ADR 0051 §7 reuses ADR
/// 0049's bounds deliberately, and the run-local supervisor's five-minute
/// readiness deadline already derives from the same number. Tests construct a
/// [`VaapiPreflightConfig`] with short clocks so expiry is reachable without
/// waiting them out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VaapiProbeClocks {
    /// Deadline for one probe encode or decode.
    pub probe_timeout: Duration,
    /// Deadline for the whole concurrent capacity probe.
    pub capacity_clock: Duration,
    /// Deadline for VAAPI preflight as a whole.
    pub readiness_deadline: Duration,
}

impl Default for VaapiProbeClocks {
    fn default() -> Self {
        Self {
            probe_timeout: PROBE_TIMEOUT,
            capacity_clock: Duration::from_mins(1),
            readiness_deadline: Duration::from_mins(5),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaapiPreflightConfig {
    pub pci_address: String,
    pub max_sessions: u32,
    pub dri_root: PathBuf,
    pub drm_sysfs_root: PathBuf,
    pub clocks: VaapiProbeClocks,
}

/// What a probe actually proved about the bound VAAPI device.
///
/// `encoders` and `decoders` list only codecs that encoded or decoded on
/// `render_node` in this process (ADR 0051 §2); nothing here is derived from
/// `FFmpeg`'s or `vainfo`'s advertised lists alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaapiPreflight {
    pub pci_address: String,
    pub render_node: PathBuf,
    pub device_name: String,
    pub driver_version: String,
    pub max_sessions: u32,
    pub encoders: Vec<String>,
    pub decoders: Vec<String>,
    pub decoder_diagnostics: Vec<String>,
}

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
    pub vaapi: Option<VaapiPreflight>,
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
    let vaapi = vaapi_config_from_process_env()?;
    match (nvidia, vaapi) {
        (Some(_), Some(_)) => Err(FFmpegPreflightError::Failed(format!(
            "{NVIDIA_DEVICE_ENV} and {VAAPI_DEVICE_ENV} are both set; one worker binds one \
             accelerator, so run one worker per device"
        ))),
        (Some(config), None) => preflight_with_nvidia(&ffmpeg_path, &ffprobe_path, &config),
        (None, Some(config)) => preflight_with_vaapi(&ffmpeg_path, &ffprobe_path, &config),
        (None, None) => preflight_with_paths(&ffmpeg_path, &ffprobe_path),
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
        vaapi: None,
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

/// Binds the worker to one VAAPI device and proves what it can do (ADR 0051).
///
/// Resolves the configured PCI address to a render node, verifies the readback,
/// then executes probes: nothing is advertised that has not encoded or decoded on
/// `config`'s device in this process. Never cached — a host driver swap moves
/// capability with no VOOM configuration change (spec §2.1), so this runs on every
/// start.
///
/// # Errors
/// Returns `FFmpegPreflightError::Failed` with a distinct message per spec §6
/// condition: a malformed or unresolvable address, an absent, occupied, or
/// unopenable node, a readback mismatch, a driver build lacking the codec, any
/// other probe-encode failure, a capacity-probe failure, or clock expiry.
pub fn preflight_with_vaapi(
    ffmpeg_path: &Path,
    ffprobe_path: &Path,
    config: &VaapiPreflightConfig,
) -> Result<FfmpegPreflight, FFmpegPreflightError> {
    if !(1..=16).contains(&config.max_sessions) {
        return Err(FFmpegPreflightError::Failed(
            "VAAPI max sessions must be in 1..=16".to_owned(),
        ));
    }
    let started = std::time::Instant::now();
    let mut preflight = preflight_with_paths(ffmpeg_path, ffprobe_path)?;
    let node = resolve_vaapi_render_node(config)?;
    require_vaapi_build_features(ffmpeg_path, config)?;
    let (device_name, driver_version) = probe_vaapi_identity(ffmpeg_path, config, &node)?;

    require_readiness_budget(started, config, "the `hevc_vaapi` probe encode")?;
    run_vaapi_encode_probe(ffmpeg_path, config, &node)?;
    let (decoders, decoder_diagnostics) = probe_vaapi_decoders(ffmpeg_path, config, &node)?;
    require_readiness_budget(started, config, "the concurrent capacity probe")?;
    prove_vaapi_capacity(ffmpeg_path, config, &node)?;

    preflight.vaapi = Some(VaapiPreflight {
        pci_address: config.pci_address.clone(),
        render_node: node,
        device_name,
        driver_version,
        max_sessions: config.max_sessions,
        encoders: vec![VAAPI_HEVC_ENCODER.to_owned()],
        decoders,
        decoder_diagnostics,
    });
    Ok(preflight)
}

/// Fails startup when the five-minute readiness deadline is spent (ADR 0051 §7).
fn require_readiness_budget(
    started: std::time::Instant,
    config: &VaapiPreflightConfig,
    stage: &str,
) -> Result<(), FFmpegPreflightError> {
    if started.elapsed() < config.clocks.readiness_deadline {
        return Ok(());
    }
    Err(FFmpegPreflightError::Failed(format!(
        "VAAPI readiness deadline of {} seconds expired before {stage} proved on PCI address `{}`",
        config.clocks.readiness_deadline.as_secs(),
        config.pci_address
    )))
}

/// `FFmpeg`'s own lists are necessary but never sufficient (ADR 0051 §2); this only
/// skips probing a build that plainly cannot try.
fn require_vaapi_build_features(
    ffmpeg_path: &Path,
    config: &VaapiPreflightConfig,
) -> Result<(), FFmpegPreflightError> {
    for (flag, required) in [
        ("-encoders", &[VAAPI_HEVC_ENCODER][..]),
        ("-filters", &["hwupload", "format"][..]),
    ] {
        let text = command_text(
            &format!("ffmpeg {flag}"),
            command_output(Command::new(ffmpeg_path).arg("-hide_banner").arg(flag)),
        )?;
        for token in required {
            if parse_token(&text, token).is_none() {
                return Err(FFmpegPreflightError::Failed(format!(
                    "ffmpeg does not advertise required VAAPI feature `{token}`, so PCI address \
                     `{}` cannot be probed; rebuild ffmpeg with --enable-vaapi --enable-libdrm",
                    config.pci_address
                )));
            }
        }
    }
    Ok(())
}

/// Reads the device name and loaded driver build off the VAAPI connection.
///
/// The driver build is the thing capability tracks (ADR 0051 §2), and it is
/// invisible from the render node, so it has to come from the connection `FFmpeg`
/// actually opened.
fn probe_vaapi_identity(
    ffmpeg_path: &Path,
    config: &VaapiPreflightConfig,
    node: &Path,
) -> Result<(String, String), FFmpegPreflightError> {
    let mut command = Command::new(ffmpeg_path);
    command.args([
        "-hide_banner",
        "-nostdin",
        "-v",
        "verbose",
        "-init_hw_device",
    ]);
    command.arg(format!("vaapi=probe:{}", node.display()));
    command.args([
        "-f",
        "lavfi",
        "-i",
        "testsrc=size=64x64:rate=1",
        "-frames:v",
        "1",
        "-an",
        "-f",
        "null",
        "-",
    ]);
    let text = command_text(
        &format!("VAAPI device identity probe on `{}`", node.display()),
        command_output_within(&mut command, config.clocks.probe_timeout),
    )?;
    let driver = text
        .lines()
        .find_map(|line| line.split_once("VAAPI driver: "))
        .map(|(_, driver)| driver.trim().trim_end_matches('.').to_owned())
        .ok_or_else(|| {
            FFmpegPreflightError::Failed(format!(
                "VAAPI render node `{}` for PCI address `{}` reported no driver string; \
                 the VA driver did not initialise",
                node.display(),
                config.pci_address
            ))
        })?;
    Ok((vaapi_device_name(&driver), driver))
}

/// Extracts the device name from a VA driver string such as
/// `Mesa Gallium driver 26.1.5 for AMD Radeon 8060S Graphics (radeonsi, …)`,
/// falling back to the whole string when a driver spells it differently.
fn vaapi_device_name(driver: &str) -> String {
    driver
        .split_once(" for ")
        .map(|(_, rest)| rest.split_once(" (").map_or(rest, |(name, _)| name).trim())
        .filter(|name| !name.is_empty())
        .unwrap_or(driver)
        .to_owned()
}

fn vaapi_encode_probe_command(ffmpeg_path: &Path, node: &Path, output: &Path) -> Command {
    let mut command = Command::new(ffmpeg_path);
    command.args(["-hide_banner", "-nostdin", "-vaapi_device"]);
    command.arg(node);
    command.args([
        "-f",
        "lavfi",
        "-i",
        "testsrc=size=256x256:rate=1",
        "-frames:v",
        "1",
        "-an",
        "-vf",
        "format=nv12,hwupload",
        "-c:v",
        VAAPI_HEVC_ENCODER,
        "-rc_mode",
        "CQP",
        "-qp",
        "23",
        "-f",
        "hevc",
        "-y",
    ]);
    command.arg(output);
    command
}

/// Spec §5's encoder probe: synthesize, upload, encode, require a non-empty file.
fn run_vaapi_encode_probe(
    ffmpeg_path: &Path,
    config: &VaapiPreflightConfig,
    node: &Path,
) -> Result<(), FFmpegPreflightError> {
    let probe_dir = ProbeDir::new("vaapi-encode-probe")?;
    let output = probe_dir.path.join("probe.hevc");
    let mut command = vaapi_encode_probe_command(ffmpeg_path, node, &output);
    let result = command_text(
        "probe encode",
        command_output_within(&mut command, config.clocks.probe_timeout),
    );
    if let Err(FFmpegPreflightError::Failed(detail)) = result {
        return Err(encode_probe_error(config, node, &detail));
    }
    let bytes = std::fs::metadata(&output).map(|metadata| metadata.len());
    if bytes.unwrap_or(0) == 0 {
        return Err(FFmpegPreflightError::Failed(format!(
            "`{VAAPI_HEVC_ENCODER}` probe encode on `{}` (PCI address `{}`) exited cleanly but \
             produced no output, so the codec is not proven",
            node.display(),
            config.pci_address
        )));
    }
    Ok(())
}

/// Splits spec §6's two probe-encode rows apart.
///
/// `No usable encoding profile found` is the stock-versus-freeworld driver split
/// (spec §2.1) and has a specific fix; everything else needs `FFmpeg`'s own error
/// surfaced instead of a guess about the driver package.
fn encode_probe_error(
    config: &VaapiPreflightConfig,
    node: &Path,
    detail: &str,
) -> FFmpegPreflightError {
    if detail.contains("No usable encoding profile found") {
        return FFmpegPreflightError::Failed(format!(
            "the loaded VA driver build cannot encode `{VAAPI_HEVC_ENCODER}` on `{}` (PCI \
             address `{}`): ffmpeg reported `No usable encoding profile found`; install a driver \
             build carrying HEVC encode (on Fedora, RPM Fusion's mesa-va-drivers-freeworld)",
            node.display(),
            config.pci_address
        ));
    }
    FFmpegPreflightError::Failed(format!(
        "`{VAAPI_HEVC_ENCODER}` probe encode on `{}` (PCI address `{}`) failed: {detail}",
        node.display(),
        config.pci_address
    ))
}

/// Spec §5's decoder probe: decode a bundled 4:2:0 fixture per codec with
/// `-hwaccel_output_format vaapi`, which errors instead of silently falling back
/// to software. A codec whose probe fails is not advertised, and its reason is
/// retained rather than dropped.
fn probe_vaapi_decoders(
    ffmpeg_path: &Path,
    config: &VaapiPreflightConfig,
    node: &Path,
) -> Result<(Vec<String>, Vec<String>), FFmpegPreflightError> {
    let probe_dir = ProbeDir::new("vaapi-decoder-probe")?;
    let mut decoders = Vec::new();
    let mut diagnostics = Vec::new();
    for codec in VAAPI_VIDEO_DECODERS {
        let fixture = probe_dir.path.join(format!("{codec}.mkv"));
        std::fs::write(&fixture, vaapi_decoder_fixture(codec)?).map_err(|error| {
            FFmpegPreflightError::Failed(format!(
                "writing `{codec}` VAAPI decoder probe fixture to `{}`: {error}",
                fixture.display()
            ))
        })?;
        match run_vaapi_decode_probe(ffmpeg_path, config, node, &fixture) {
            Ok(()) => decoders.push((*codec).to_owned()),
            Err(error) => diagnostics.push(format!("{codec}: {error}")),
        }
    }
    Ok((decoders, diagnostics))
}

/// The committed fixtures are `yuv420p`: spec §2.3 records that the obvious
/// `testsrc`-to-`libx265` recipe yields `gbrp`, which VAAPI cannot decode, so they
/// cannot be synthesized at probe time.
fn vaapi_decoder_fixture(codec: &str) -> Result<&'static [u8], FFmpegPreflightError> {
    match codec {
        "h264" => Ok(include_bytes!("../tests/fixtures/vaapi-probe-h264.mkv")),
        "hevc" => Ok(include_bytes!("../tests/fixtures/vaapi-probe-hevc.mkv")),
        "av1" => Ok(include_bytes!("../tests/fixtures/vaapi-probe-av1.mkv")),
        other => Err(FFmpegPreflightError::Failed(format!(
            "VAAPI decoder probe has no bundled `{other}` fixture; add one generated with an \
             explicit -pix_fmt yuv420p"
        ))),
    }
}

fn run_vaapi_decode_probe(
    ffmpeg_path: &Path,
    config: &VaapiPreflightConfig,
    node: &Path,
    fixture: &Path,
) -> Result<(), FFmpegPreflightError> {
    let mut command = Command::new(ffmpeg_path);
    command.args([
        "-hide_banner",
        "-nostdin",
        "-hwaccel",
        "vaapi",
        "-hwaccel_device",
    ]);
    command.arg(node);
    command.args(["-hwaccel_output_format", "vaapi", "-i"]);
    command.arg(fixture);
    command.args(["-frames:v", "1", "-an", "-f", "null", "-"]);
    command_text(
        "exact-device smoke decode",
        command_output_within(&mut command, config.clocks.probe_timeout),
    )
    .map(|_| ())
}

/// Proves the operator's declaration with that many concurrent probe encodes,
/// bounded by ADR 0051 §7's one-minute capacity clock.
///
/// A failure is always reported as diagnostic uncertainty: VAAPI exposes no
/// encoder-session enumeration, so ADR 0049 §3's external-contention/VOOM-orphan
/// distinction has no counterpart here and never will (ADR 0051 §6).
fn prove_vaapi_capacity(
    ffmpeg_path: &Path,
    config: &VaapiPreflightConfig,
    node: &Path,
) -> Result<(), FFmpegPreflightError> {
    let probe_dir = ProbeDir::new("vaapi-capacity-probe")?;
    let started = std::time::Instant::now();
    let mut children = Vec::new();
    for session in 0..config.max_sessions {
        let output = probe_dir.path.join(format!("session-{session}.hevc"));
        let mut command = vaapi_encode_probe_command(ffmpeg_path, node, &output);
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        match command.spawn() {
            Ok(child) => children.push(child),
            Err(error) => {
                kill_and_reap_all(&mut children);
                return Err(capacity_probe_error(config, node, &error.to_string()));
            }
        }
    }
    while let Some(child) = children.pop() {
        let remaining = config
            .clocks
            .capacity_clock
            .saturating_sub(started.elapsed());
        let result = wait_child_output(child, remaining, "concurrent probe encode")
            .and_then(|output| command_text("concurrent probe encode", Ok(output)));
        if let Err(FFmpegPreflightError::Failed(detail)) = result {
            kill_and_reap_all(&mut children);
            return Err(capacity_probe_error(config, node, &detail));
        }
    }
    Ok(())
}

fn capacity_probe_error(
    config: &VaapiPreflightConfig,
    node: &Path,
    detail: &str,
) -> FFmpegPreflightError {
    FFmpegPreflightError::Failed(format!(
        "VAAPI capacity probe for {} concurrent `{VAAPI_HEVC_ENCODER}` encodes on `{}` (PCI \
         address `{}`) failed: {detail}. VAAPI exposes no encoder-session enumeration, so the \
         cause cannot be attributed; lower the declared capacity or retry when the device is \
         known idle",
        config.max_sessions,
        node.display(),
        config.pci_address
    ))
}

fn vaapi_config_from_process_env() -> Result<Option<VaapiPreflightConfig>, FFmpegPreflightError> {
    let sessions = std::env::var(VAAPI_MAX_SESSIONS_ENV).ok();
    vaapi_config_from_env_values(
        std::env::var(VAAPI_DEVICE_ENV).ok(),
        sessions.as_deref(),
        std::env::var_os(DRI_ROOT_ENV),
        std::env::var_os(DRM_SYSFS_ROOT_ENV),
    )
}

/// Builds the VAAPI configuration from raw environment values.
///
/// Split from [`vaapi_config_from_process_env`] so the declaration bounds and the
/// PCI-address rule are testable without mutating the test process's environment.
fn vaapi_config_from_env_values(
    device: Option<String>,
    sessions: Option<&str>,
    dri_root: Option<OsString>,
    drm_sysfs_root: Option<OsString>,
) -> Result<Option<VaapiPreflightConfig>, FFmpegPreflightError> {
    let Some(pci_address) = device else {
        if sessions.is_some() {
            return Err(FFmpegPreflightError::Failed(format!(
                "{VAAPI_MAX_SESSIONS_ENV} requires {VAAPI_DEVICE_ENV}"
            )));
        }
        return Ok(None);
    };
    validate_pci_address(&pci_address)?;
    let max_sessions = sessions.unwrap_or("1").parse::<u32>().map_err(|error| {
        FFmpegPreflightError::Failed(format!(
            "{VAAPI_MAX_SESSIONS_ENV} must be an integer in 1..=16: {error}"
        ))
    })?;
    if !(1..=16).contains(&max_sessions) {
        return Err(FFmpegPreflightError::Failed(format!(
            "{VAAPI_MAX_SESSIONS_ENV} must be in 1..=16"
        )));
    }
    Ok(Some(VaapiPreflightConfig {
        pci_address,
        max_sessions,
        dri_root: dri_root.map_or_else(|| PathBuf::from(DEFAULT_DRI_ROOT), PathBuf::from),
        drm_sysfs_root: drm_sysfs_root
            .map_or_else(|| PathBuf::from(DEFAULT_DRM_SYSFS_ROOT), PathBuf::from),
        clocks: VaapiProbeClocks::default(),
    }))
}

/// Rejects anything that is not a domain-qualified lowercase PCI address.
///
/// Spec §4: configuration accepts `0000:f4:00.0` and nothing else. A render-node
/// path or an ordinal is an enumeration-order artifact that can renumber across a
/// reboot, so accepting either would hand the worker an identity it cannot keep.
fn validate_pci_address(pci_address: &str) -> Result<(), FFmpegPreflightError> {
    let reject = || {
        FFmpegPreflightError::Failed(format!(
            "VAAPI device must be a PCI address like `0000:f4:00.0`, not `{pci_address}`: \
             render-node paths and ordinals renumber, so they are not accepted"
        ))
    };
    let Some((domain, rest)) = pci_address.split_once(':') else {
        return Err(reject());
    };
    let Some((bus, device_function)) = rest.split_once(':') else {
        return Err(reject());
    };
    let Some((device, function)) = device_function.split_once('.') else {
        return Err(reject());
    };
    let hex = |text: &str, width: usize| {
        text.len() == width && text.bytes().all(|byte| byte.is_ascii_hexdigit())
    };
    let lowercase = |text: &str| !text.bytes().any(|byte| byte.is_ascii_uppercase());
    if !hex(domain, 4)
        || !hex(bus, 2)
        || !hex(device, 2)
        || !hex(function, 1)
        || !lowercase(pci_address)
    {
        return Err(reject());
    }
    Ok(())
}

/// Resolves the configured PCI address to a usable render node (spec §4 steps 1-2).
///
/// Each failure carries its own diagnostic because each has a different fix: a
/// wrong address, a departed device, an occupied path, a missing group
/// membership, and a stale udev symlink are five different operator actions.
fn resolve_vaapi_render_node(
    config: &VaapiPreflightConfig,
) -> Result<PathBuf, FFmpegPreflightError> {
    validate_pci_address(&config.pci_address)?;
    let by_path = config
        .dri_root
        .join("by-path")
        .join(format!("pci-{}-render", config.pci_address));
    if std::fs::symlink_metadata(&by_path).is_err() {
        return Err(FFmpegPreflightError::Failed(format!(
            "PCI address `{}` has no VAAPI render node: `{}` does not exist; \
             confirm the address with `lspci -D` and that a DRM driver bound the device",
            config.pci_address,
            by_path.display()
        )));
    }
    let node = std::fs::canonicalize(&by_path).map_err(|error| {
        FFmpegPreflightError::Failed(format!(
            "VAAPI render node is absent for PCI address `{}`: `{}` does not resolve ({error}); \
             the device may have been removed or the driver unbound",
            config.pci_address,
            by_path.display()
        ))
    })?;
    require_openable_render_node(&node, &config.pci_address)?;
    verify_pci_readback(config, &node, &by_path)?;
    Ok(node)
}

fn require_openable_render_node(
    node: &Path,
    pci_address: &str,
) -> Result<(), FFmpegPreflightError> {
    let metadata = std::fs::metadata(node).map_err(|error| {
        FFmpegPreflightError::Failed(format!(
            "VAAPI render node is absent for PCI address `{pci_address}`: \
             cannot stat `{}` ({error})",
            node.display()
        ))
    })?;
    if let Err(error) = std::fs::File::open(node) {
        if error.kind() == io::ErrorKind::PermissionDenied {
            return Err(FFmpegPreflightError::Failed(format!(
                "permission denied opening VAAPI render node `{}` for PCI address \
                 `{pci_address}`: add the worker's user to the `render` group (or `video` on \
                 hosts that own the node that way)",
                node.display()
            )));
        }
        return Err(FFmpegPreflightError::Failed(format!(
            "cannot open VAAPI render node `{}` for PCI address `{pci_address}`: {error}",
            node.display()
        )));
    }
    if !is_character_device(&metadata) {
        return Err(FFmpegPreflightError::Failed(format!(
            "VAAPI render node `{}` for PCI address `{pci_address}` is not a character device; \
             something other than a DRM render node occupies that path",
            node.display()
        )));
    }
    Ok(())
}

#[cfg(unix)]
fn is_character_device(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::FileTypeExt;

    metadata.file_type().is_char_device()
}

#[cfg(not(unix))]
fn is_character_device(_metadata: &std::fs::Metadata) -> bool {
    false
}

/// Spec §4 step 2: read the resolved node's own PCI address back and compare.
///
/// udev generates the `by-path` symlink from the very address this re-reads, so a
/// disagreement means the symlink is stale or hand-made. This is not proof that an
/// encode ran on the intended device — the §5 probe is (ADR 0051 §1).
fn verify_pci_readback(
    config: &VaapiPreflightConfig,
    node: &Path,
    by_path: &Path,
) -> Result<(), FFmpegPreflightError> {
    let node_name = node.file_name().ok_or_else(|| {
        FFmpegPreflightError::Failed(format!(
            "VAAPI render node `{}` has no file name to read its PCI address back from",
            node.display()
        ))
    })?;
    let device_link = config.drm_sysfs_root.join(node_name).join("device");
    let device = std::fs::canonicalize(&device_link).map_err(|error| {
        FFmpegPreflightError::Failed(format!(
            "cannot read the PCI address of VAAPI render node `{}` back: `{}` does not resolve \
             ({error})",
            node.display(),
            device_link.display()
        ))
    })?;
    let observed = device
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    if observed != config.pci_address {
        return Err(FFmpegPreflightError::Failed(format!(
            "VAAPI render node `{}` reports PCI address `{observed}` but configuration names \
             `{}`: the `{}` symlink is stale; re-run `udevadm trigger` or correct the \
             configured address",
            node.display(),
            config.pci_address,
            by_path.display()
        )));
    }
    Ok(())
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
            return Err(FFmpegPreflightError::Failed(format!(
                "NVIDIA decoder probe has no fixture encoder for `{codec}`"
            )));
        };
        let fixture = probe_dir.path.join(format!("{codec}.mkv"));
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
        match command.spawn() {
            Ok(child) => children.push(child),
            Err(error) => {
                kill_and_reap_all(&mut children);
                return Err(FFmpegPreflightError::Failed(format!(
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

/// A private temporary directory for probe inputs and outputs, removed on every
/// success and failure path (ADR 0049 §9).
struct ProbeDir {
    path: PathBuf,
}

impl ProbeDir {
    fn new(label: &str) -> Result<Self, FFmpegPreflightError> {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| {
                FFmpegPreflightError::Failed(format!(
                    "system clock before Unix epoch during {label}: {error}"
                ))
            })?
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("voom-{label}-{}-{nonce}", std::process::id()));
        std::fs::create_dir(&path).map_err(|error| {
            FFmpegPreflightError::Failed(format!(
                "create {label} directory {}: {error}",
                path.display()
            ))
        })?;
        Ok(Self { path })
    }
}

impl Drop for ProbeDir {
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
    command_output_within(command, PROBE_TIMEOUT)
}

/// [`command_output`] under an explicit deadline, so ADR 0051 §7's per-probe clock
/// is a value a test can shorten rather than a constant it has to wait out.
fn command_output_within(command: &mut Command, deadline: Duration) -> io::Result<Output> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut attempts_remaining = 3;
    loop {
        attempts_remaining -= 1;
        match command
            .spawn()
            .and_then(|child| wait_child_output_io(child, deadline, "dependency probe"))
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

/// Waits for `child` under `deadline`, draining its piped output the whole time.
///
/// The drain threads are not an optimization: a child that writes more than the OS
/// pipe capacity blocks in `write()` until someone reads, so polling `try_wait()`
/// against an undrained pipe can never see it exit.
fn wait_child_output_io(mut child: Child, deadline: Duration, label: &str) -> io::Result<Output> {
    let stdout = drain_in_background(child.stdout.take());
    let stderr = drain_in_background(child.stderr.take());
    let started = std::time::Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Output {
                status,
                stdout: join_drained(stdout)?,
                stderr: join_drained(stderr)?,
            });
        }
        if started.elapsed() >= deadline {
            kill_and_reap(&mut child);
            // Deliberately detached, not joined. Killing the child does not close a
            // pipe that a surviving grandchild still holds, so joining here would
            // block for exactly as long as the deadline exists to prevent. The
            // threads exit on their own once the last writer does.
            drop(stdout);
            drop(stderr);
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("{label} exceeded {} seconds", deadline.as_secs()),
            ));
        }
        thread::sleep(Duration::from_millis(10));
    }
}

type DrainHandle = Option<thread::JoinHandle<io::Result<Vec<u8>>>>;

fn drain_in_background<R>(stream: Option<R>) -> DrainHandle
where
    R: Read + Send + 'static,
{
    stream.map(|mut stream| {
        thread::spawn(move || {
            let mut buffer = Vec::new();
            stream.read_to_end(&mut buffer).map(|_| buffer)
        })
    })
}

fn join_drained(handle: DrainHandle) -> io::Result<Vec<u8>> {
    match handle {
        None => Ok(Vec::new()),
        Some(handle) => handle
            .join()
            .map_err(|_| io::Error::other("output drain thread panicked"))?,
    }
}

fn kill_and_reap(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn kill_and_reap_all(children: &mut Vec<Child>) {
    for child in &mut *children {
        kill_and_reap(child);
    }
    children.clear();
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
