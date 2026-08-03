use std::{
    ffi::OsString,
    io,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

use voom_core::VAAPI_VIDEO_DECODERS;
use voom_worker_protocol::VAAPI_READINESS_DEADLINE;

use super::{
    FFmpegPreflightError, FfmpegPreflight, preflight_with_paths,
    process::{
        PROBE_TIMEOUT, ProbeDir, command_output, command_output_within, command_text,
        kill_and_reap_all, parse_token, wait_child_output,
    },
};

pub(super) const VAAPI_HEVC_ENCODER: &str = "hevc_vaapi";

pub const VAAPI_DEVICE_ENV: &str = "VOOM_VAAPI_DEVICE";

pub const VAAPI_MAX_SESSIONS_ENV: &str = "VOOM_VAAPI_MAX_SESSIONS";

pub const DRI_ROOT_ENV: &str = "VOOM_DRI_ROOT";

pub const DRM_SYSFS_ROOT_ENV: &str = "VOOM_DRM_SYSFS_ROOT";

const DEFAULT_DRI_ROOT: &str = "/dev/dri";

const DEFAULT_DRM_SYSFS_ROOT: &str = "/sys/class/drm";

/// The three clocks ADR 0052 §7 adopts unchanged from ADR 0049 §3.
///
/// They are constants rather than operator configuration: ADR 0052 §7 reuses ADR
/// 0049's bounds deliberately. `readiness_deadline` is
/// [`VAAPI_READINESS_DEADLINE`], the same constant the run-local supervisor adds
/// its coordination allowance to, so this expiry — which names the stage that did
/// not prove — is always reached before the supervisor abandons the process. Tests
/// construct a [`VaapiPreflightConfig`] with short clocks so expiry is reachable
/// without waiting them out.
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
            readiness_deadline: VAAPI_READINESS_DEADLINE,
        }
    }
}

/// VAAPI device binding, declared capacity, filesystem roots, and probe clocks.
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
/// `render_node` in this process (ADR 0052 §2); nothing here is derived from
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

/// Binds the worker to one VAAPI device and proves what it can do (ADR 0052).
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

/// Fails startup when the five-minute readiness deadline is spent (ADR 0052 §7).
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

/// `FFmpeg`'s own lists are necessary but never sufficient (ADR 0052 §2); this only
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
/// The driver build is the thing capability tracks (ADR 0052 §2), and it is
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
    let output = probe_dir.path().join("probe.hevc");
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
        let fixture = probe_dir.path().join(format!("{codec}.mkv"));
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
        "h264" => Ok(include_bytes!("../../tests/fixtures/vaapi-probe-h264.mkv")),
        "hevc" => Ok(include_bytes!("../../tests/fixtures/vaapi-probe-hevc.mkv")),
        "av1" => Ok(include_bytes!("../../tests/fixtures/vaapi-probe-av1.mkv")),
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
/// bounded by ADR 0052 §7's one-minute capacity clock.
///
/// A failure is always reported as diagnostic uncertainty: VAAPI exposes no
/// encoder-session enumeration, so ADR 0049 §3's external-contention/VOOM-orphan
/// distinction has no counterpart here and never will (ADR 0052 §6).
fn prove_vaapi_capacity(
    ffmpeg_path: &Path,
    config: &VaapiPreflightConfig,
    node: &Path,
) -> Result<(), FFmpegPreflightError> {
    let probe_dir = ProbeDir::new("vaapi-capacity-probe")?;
    let started = std::time::Instant::now();
    let mut children = Vec::new();
    for session in 0..config.max_sessions {
        let output = probe_dir.path().join(format!("session-{session}.hevc"));
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

/// Builds the VAAPI configuration from raw environment values.
///
/// Split from `vaapi_config_from_process_env` so the declaration bounds and the
/// PCI-address rule are testable without mutating the test process's environment.
pub(super) fn vaapi_config_from_env_values(
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
    let Some([domain, bus, device, function]) = pci_address_components(pci_address) else {
        return Err(reject());
    };
    if !valid_hex_component(domain, 4)
        || !valid_hex_component(bus, 2)
        || !valid_hex_component(device, 2)
        || !valid_hex_component(function, 1)
        || pci_address.bytes().any(|byte| byte.is_ascii_uppercase())
    {
        return Err(reject());
    }
    Ok(())
}

fn pci_address_components(pci_address: &str) -> Option<[&str; 4]> {
    let (domain, rest) = pci_address.split_once(':')?;
    let (bus, device_function) = rest.split_once(':')?;
    let (device, function) = device_function.split_once('.')?;
    Some([domain, bus, device, function])
}

fn valid_hex_component(component: &str, width: usize) -> bool {
    component.len() == width && component.bytes().all(|byte| byte.is_ascii_hexdigit())
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
/// encode ran on the intended device — the §5 probe is (ADR 0052 §1).
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

#[cfg(test)]
#[path = "vaapi_test.rs"]
mod tests;
