use std::path::{Path, PathBuf};

use super::*;

// ---- VAAPI device identity (spec §6 diagnostics 1-5, ADR 0052 §1) ----
//
// Every one of these runs against a fake `/dev/dri` and a fake `/sys/class/drm`
// built in a tempdir, because the acceptance host's real device disagrees with
// most of the fixtures: without the roots being injectable, five of spec §6's
// nine diagnostics could only be reached by physically breaking the host.

#[cfg(unix)]
struct FakeDri {
    dri_root: PathBuf,
    drm_sysfs_root: PathBuf,
    pci_devices: PathBuf,
}

#[cfg(unix)]
impl FakeDri {
    fn new(root: &Path) -> Self {
        let fake = Self {
            dri_root: root.join("dev-dri"),
            drm_sysfs_root: root.join("sys-class-drm"),
            pci_devices: root.join("sys-devices"),
        };
        std::fs::create_dir_all(fake.dri_root.join("by-path")).unwrap();
        std::fs::create_dir_all(&fake.drm_sysfs_root).unwrap();
        std::fs::create_dir_all(&fake.pci_devices).unwrap();
        fake
    }

    fn config(&self, pci_address: &str) -> VaapiPreflightConfig {
        VaapiPreflightConfig {
            pci_address: pci_address.to_owned(),
            max_sessions: 1,
            dri_root: self.dri_root.clone(),
            drm_sysfs_root: self.drm_sysfs_root.clone(),
            clocks: VaapiProbeClocks::default(),
        }
    }

    /// Mimics udev's `by-path/pci-<addr>-render` symlink.
    fn link_by_path(&self, pci_address: &str, target: &Path) {
        std::os::unix::fs::symlink(
            target,
            self.dri_root
                .join("by-path")
                .join(format!("pci-{pci_address}-render")),
        )
        .unwrap();
    }

    /// Mimics `/sys/class/drm/<node>/device -> ../../devices/…/<addr>`, which is
    /// where the §4 step-2 readback reads the node's own PCI address from.
    fn link_sysfs_device(&self, node_name: &str, pci_address: &str) {
        let device = self.pci_devices.join(pci_address);
        std::fs::create_dir_all(&device).unwrap();
        let node_dir = self.drm_sysfs_root.join(node_name);
        std::fs::create_dir_all(&node_dir).unwrap();
        std::os::unix::fs::symlink(device, node_dir.join("device")).unwrap();
    }
}

/// `/dev/null` stands in for a render node: the resolution path requires a real
/// character device, and it is the only one an unprivileged test can point at.
#[cfg(unix)]
const FAKE_RENDER_NODE: &str = "/dev/null";

/// Configuration accepts a PCI address and nothing else (spec §4). A render-node
/// path renumbers and an ordinal is enumeration order, so accepting either would
/// give the worker an identity that cannot survive a reboot.
#[test]
fn vaapi_device_must_be_a_pci_address_not_a_node_path_or_ordinal() {
    assert!(validate_pci_address("0000:f4:00.0").is_ok());

    for invalid in [
        "/dev/dri/renderD128",
        "renderD128",
        "0",
        "1",
        "f4:00.0",
        "0000:f4:00",
        "0000:F4:00.0",
        "",
    ] {
        let error = validate_pci_address(invalid).unwrap_err().to_string();
        assert!(
            error.contains("must be a PCI address"),
            "`{invalid}` must be rejected with the PCI-address diagnostic, got: {error}"
        );
    }
}

/// Diagnostic 2: the address parses but udev never created an entry for it, so
/// the operator mistyped the address or the driver never bound the device.
#[cfg(unix)]
#[test]
fn vaapi_resolution_reports_an_unresolvable_pci_address() {
    let temp = tempfile::tempdir().unwrap();
    let fake = FakeDri::new(temp.path());

    let error = resolve_vaapi_render_node(&fake.config("0000:f4:00.0"))
        .unwrap_err()
        .to_string();

    assert!(
        error.contains("has no VAAPI render node"),
        "an absent by-path entry must be reported as an unresolvable address: {error}"
    );
    assert!(error.contains("0000:f4:00.0"), "{error}");
}

/// Diagnostic 3a: the by-path entry exists but dangles. Distinct from diagnostic
/// 2 because the fix differs — the device went away rather than never existing.
#[cfg(unix)]
#[test]
fn vaapi_resolution_reports_an_absent_render_node() {
    let temp = tempfile::tempdir().unwrap();
    let fake = FakeDri::new(temp.path());
    fake.link_by_path("0000:f4:00.0", &temp.path().join("gone"));

    let error = resolve_vaapi_render_node(&fake.config("0000:f4:00.0"))
        .unwrap_err()
        .to_string();

    assert!(
        error.contains("VAAPI render node is absent"),
        "a dangling by-path entry must be reported as an absent node: {error}"
    );
}

/// Diagnostic 3b: something occupies the resolved path but it is not a device, so
/// `-vaapi_device` would fail deep inside `FFmpeg` instead of at startup.
#[cfg(unix)]
#[test]
fn vaapi_resolution_reports_a_resolved_path_that_is_not_a_device() {
    let temp = tempfile::tempdir().unwrap();
    let fake = FakeDri::new(temp.path());
    let regular = temp.path().join("not-a-device");
    std::fs::write(&regular, b"").unwrap();
    fake.link_by_path("0000:f4:00.0", &regular);

    let error = resolve_vaapi_render_node(&fake.config("0000:f4:00.0"))
        .unwrap_err()
        .to_string();

    assert!(
        error.contains("is not a character device"),
        "a non-device target must be reported as such: {error}"
    );
}

/// Diagnostic 4: the node exists but the worker's user cannot open it, which on a
/// real host means missing `render`/`video` group membership. The message has to
/// say that, because "permission denied" alone does not tell an operator what to
/// change. Assumes a non-root test user; root cannot observe `EACCES`.
#[cfg(unix)]
#[test]
fn vaapi_resolution_reports_permission_denied_on_the_render_node() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    let fake = FakeDri::new(temp.path());
    let unreadable = temp.path().join("unreadable-node");
    std::fs::write(&unreadable, b"").unwrap();
    std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o000)).unwrap();
    fake.link_by_path("0000:f4:00.0", &unreadable);

    let error = resolve_vaapi_render_node(&fake.config("0000:f4:00.0"))
        .unwrap_err()
        .to_string();

    assert!(
        error.contains("permission denied opening VAAPI render node"),
        "an unopenable node must be reported as a permission failure: {error}"
    );
    assert!(
        error.contains("render") && error.contains("video"),
        "the fix is group membership, so the message must name both groups: {error}"
    );
}

/// Diagnostic 5: spec §4 step 2. udev derives the symlink name from the very
/// address this check re-reads, so a disagreement means the symlink is stale and
/// the worker would bind a device the scheduler did not choose.
#[cfg(unix)]
#[test]
fn vaapi_resolution_reports_a_pci_readback_mismatch() {
    let temp = tempfile::tempdir().unwrap();
    let fake = FakeDri::new(temp.path());
    fake.link_by_path("0000:f4:00.0", Path::new(FAKE_RENDER_NODE));
    fake.link_sysfs_device("null", "0000:aa:00.0");

    let error = resolve_vaapi_render_node(&fake.config("0000:f4:00.0"))
        .unwrap_err()
        .to_string();

    assert!(
        error.contains("reports PCI address"),
        "a readback disagreement must be reported as a mismatch: {error}"
    );
    assert!(
        error.contains("0000:aa:00.0") && error.contains("0000:f4:00.0"),
        "the message must name both the observed and the configured address: {error}"
    );
}

#[cfg(unix)]
#[test]
fn vaapi_resolution_accepts_a_node_whose_readback_matches() {
    let temp = tempfile::tempdir().unwrap();
    let fake = FakeDri::new(temp.path());
    fake.link_by_path("0000:f4:00.0", Path::new(FAKE_RENDER_NODE));
    fake.link_sysfs_device("null", "0000:f4:00.0");

    let node = resolve_vaapi_render_node(&fake.config("0000:f4:00.0")).unwrap();

    assert_eq!(node, Path::new(FAKE_RENDER_NODE));
}

// ---- VAAPI probe-proven capability (spec §6 diagnostics 6-9, ADR 0052 §2/§6/§7) ----

/// A fake `ffmpeg` whose `hevc_vaapi` encode and `-hwaccel` decode arms are
/// supplied per test. Probes are the only thing that can prove capability
/// (ADR 0052 §2), so faking the binary is the only way to exercise their failure
/// modes on a host whose real driver succeeds.
#[cfg(unix)]
fn vaapi_ffmpeg_stub(dir: &Path, encode_arm: &str, decode_arm: &str) -> PathBuf {
    let encoders = format!("{ALL_ENCODERS} V..... hevc_vaapi H.265/HEVC (VAAPI)\n");
    let body = format!(
        "#!/bin/sh\n\
         for a in \"$@\"; do last=\"$a\"; done\n\
         case \"$*\" in\n\
         \x20 *-version*) echo 'ffmpeg version 8.1.2' ;;\n\
         \x20 *-encoders*) cat <<'EOF'\n{encoders}EOF\n    ;;\n\
         \x20 *-filters*) echo ' ... hwupload upload'; echo ' ... format format' ;;\n\
         \x20 *-muxers*) cat <<'EOF'\n{ALL_MUXERS}EOF\n    ;;\n\
         \x20 *-init_hw_device*) echo '[VAAPI @ 0x1] VAAPI driver: Mesa Gallium driver 26.1.5 \
for AMD Radeon 8060S Graphics (radeonsi, strix_halo, ACO, DRM 3.64).' >&2 ;;\n\
         \x20 *-hwaccel*) case \"$*\" in \
           *'-hwaccel vaapi -hwaccel_device /dev/null -hwaccel_output_format vaapi'*) \
             {decode_arm} ;; \
           *) echo 'VAAPI decode was not exact-device hardware-only' >&2; exit 64 ;; \
         esac ;;\n\
         \x20 *hevc_vaapi*) case \"$*\" in \
           *'-vaapi_device /dev/null'*'-vf format=nv12,hwupload'*) {encode_arm} ;; \
           *) echo 'VAAPI encode was not exact-device hwupload' >&2; exit 64 ;; \
         esac ;;\n\
         \x20 *) exit 2 ;;\n\
         esac\n"
    );
    stub_bin(dir, "ffmpeg", &body)
}

#[cfg(unix)]
const ENCODE_OK: &str = "printf 'hevcbits' > \"$last\"";

/// Builds a fake device tree whose readback matches, so any failure the test then
/// observes comes from the probe rather than from identity.
#[cfg(unix)]
fn proven_device(root: &Path) -> (FakeDri, VaapiPreflightConfig) {
    let fake = FakeDri::new(root);
    fake.link_by_path("0000:f4:00.0", Path::new(FAKE_RENDER_NODE));
    fake.link_sysfs_device("null", "0000:f4:00.0");
    let config = fake.config("0000:f4:00.0");
    (fake, config)
}

/// A codec that has encoded on the bound device is advertised; the device name and
/// driver build come from the VAAPI connection, because ADR 0052 §2 makes the
/// loaded driver build — not the hardware — the thing capability tracks.
#[cfg(unix)]
#[test]
fn vaapi_preflight_advertises_only_probe_proven_codecs() {
    let temp = tempfile::tempdir().unwrap();
    let (_fake, config) = proven_device(temp.path());
    let ffmpeg = vaapi_ffmpeg_stub(temp.path(), ENCODE_OK, "exit 0");
    let ffprobe = fake_ffprobe(temp.path());

    let report = preflight_with_vaapi(&ffmpeg, &ffprobe, &config).unwrap();
    let vaapi = report.vaapi.unwrap();

    assert_eq!(vaapi.pci_address, "0000:f4:00.0");
    assert_eq!(vaapi.render_node, Path::new(FAKE_RENDER_NODE));
    assert_eq!(vaapi.encoders, vec!["hevc_vaapi".to_owned()]);
    assert_eq!(vaapi.decoders, vec!["h264", "hevc", "av1"]);
    assert_eq!(vaapi.device_name, "AMD Radeon 8060S Graphics");
    assert!(
        vaapi
            .driver_version
            .starts_with("Mesa Gallium driver 26.1.5"),
        "driver build must be recorded: {}",
        vaapi.driver_version
    );
    assert!(vaapi.decoder_diagnostics.is_empty());
}

/// A decoder that fails its probe is not advertised, but the reason is kept: a
/// silently shortened decoder list would look like a driver that never had the
/// codec (ADR 0052 §2).
#[cfg(unix)]
#[test]
fn vaapi_preflight_drops_unproven_decoders_and_keeps_the_reason() {
    let temp = tempfile::tempdir().unwrap();
    let (_fake, config) = proven_device(temp.path());
    let ffmpeg = vaapi_ffmpeg_stub(
        temp.path(),
        ENCODE_OK,
        "case \"$*\" in *av1*) echo 'no decode' >&2; exit 1 ;; *) exit 0 ;; esac",
    );
    let ffprobe = fake_ffprobe(temp.path());

    let vaapi = preflight_with_vaapi(&ffmpeg, &ffprobe, &config)
        .unwrap()
        .vaapi
        .unwrap();

    assert_eq!(vaapi.decoders, vec!["h264", "hevc"]);
    assert_eq!(vaapi.decoder_diagnostics.len(), 1);
    assert!(
        vaapi.decoder_diagnostics[0].contains("av1"),
        "the retained diagnostic must name the codec: {:?}",
        vaapi.decoder_diagnostics
    );
}

/// Diagnostic 6: the observed `No usable encoding profile found`. This is the
/// stock-versus-freeworld driver split (spec §2.1), and it is the one probe
/// failure with a specific operator action, so it must not read like a generic
/// encode error.
#[cfg(unix)]
#[test]
fn vaapi_preflight_reports_a_driver_build_without_the_codec() {
    let temp = tempfile::tempdir().unwrap();
    let (_fake, config) = proven_device(temp.path());
    let ffmpeg = vaapi_ffmpeg_stub(
        temp.path(),
        "echo '[hevc_vaapi @ 0x1] No usable encoding profile found.' >&2; exit 218",
        "exit 0",
    );
    let ffprobe = fake_ffprobe(temp.path());

    let error = preflight_with_vaapi(&ffmpeg, &ffprobe, &config)
        .unwrap_err()
        .to_string();

    assert!(
        error.contains("loaded VA driver build cannot encode `hevc_vaapi`"),
        "the driver-build failure needs its own diagnostic: {error}"
    );
    assert!(
        error.contains("mesa-va-drivers-freeworld"),
        "the message must name the package that supplies HEVC encode: {error}"
    );
}

/// Diagnostic 7: any other probe-encode failure. Distinct from diagnostic 6
/// because the operator action is to read `FFmpeg`'s own error, not to swap drivers.
#[cfg(unix)]
#[test]
fn vaapi_preflight_reports_a_probe_encode_failure_for_any_other_reason() {
    let temp = tempfile::tempdir().unwrap();
    let (_fake, config) = proven_device(temp.path());
    let ffmpeg = vaapi_ffmpeg_stub(temp.path(), "echo 'Invalid argument' >&2; exit 1", "exit 0");
    let ffprobe = fake_ffprobe(temp.path());

    let error = preflight_with_vaapi(&ffmpeg, &ffprobe, &config)
        .unwrap_err()
        .to_string();

    assert!(
        error.contains("`hevc_vaapi` probe encode on"),
        "a generic probe failure must name the probe and the device: {error}"
    );
    assert!(
        error.contains("Invalid argument"),
        "FFmpeg's own error must survive into the diagnostic: {error}"
    );
    assert!(
        !error.contains("mesa-va-drivers-freeworld"),
        "a generic failure must not be blamed on the driver build: {error}"
    );
}

/// An encoder that exits zero but writes nothing has not encoded anything. Spec
/// §5 requires a non-empty output precisely because a clean exit is not proof.
#[cfg(unix)]
#[test]
fn vaapi_preflight_rejects_a_probe_encode_that_produced_no_output() {
    let temp = tempfile::tempdir().unwrap();
    let (_fake, config) = proven_device(temp.path());
    let ffmpeg = vaapi_ffmpeg_stub(temp.path(), ": > \"$last\"", "exit 0");
    let ffprobe = fake_ffprobe(temp.path());

    let error = preflight_with_vaapi(&ffmpeg, &ffprobe, &config)
        .unwrap_err()
        .to_string();

    assert!(
        error.contains("produced no output"),
        "an empty output must fail the probe: {error}"
    );
}

/// Diagnostic 8: VAAPI has no encoder-session enumeration (ADR 0052 §6), so the
/// message must state the uncertainty and must never claim external contention —
/// there is nothing on the device that could tell it apart.
#[cfg(unix)]
#[test]
fn vaapi_capacity_probe_failure_reports_diagnostic_uncertainty() {
    let temp = tempfile::tempdir().unwrap();
    let (_fake, mut config) = proven_device(temp.path());
    config.max_sessions = 3;
    let counter = temp.path().join("encode-count");
    let ffmpeg = vaapi_ffmpeg_stub(
        temp.path(),
        &format!(
            "printf x >> {counter}; if [ \"$(wc -c < {counter})\" -gt 1 ]; then \
             echo 'device busy' >&2; exit 1; fi; {ENCODE_OK}",
            counter = counter.display()
        ),
        "exit 0",
    );
    let ffprobe = fake_ffprobe(temp.path());

    let error = preflight_with_vaapi(&ffmpeg, &ffprobe, &config)
        .unwrap_err()
        .to_string();

    assert!(
        error.contains("VAAPI capacity probe for 3 concurrent"),
        "the message must name the declaration that did not prove: {error}"
    );
    assert!(
        error.contains("cannot be attributed"),
        "ADR 0052 §6 forbids attributing a VAAPI capacity failure: {error}"
    );
    assert!(
        !error.contains("contention"),
        "external contention is exactly the cause VAAPI cannot distinguish: {error}"
    );
}

/// Diagnostic 9a: a hung probe encode expires the per-probe clock and names the
/// codec, so a wedged driver fails startup instead of leaving the worker pending
/// (ADR 0052 §7).
#[cfg(unix)]
#[test]
fn vaapi_probe_encode_expiry_names_the_codec_that_did_not_prove() {
    let temp = tempfile::tempdir().unwrap();
    let (_fake, mut config) = proven_device(temp.path());
    config.clocks.probe_timeout = Duration::from_millis(150);
    let ffmpeg = vaapi_ffmpeg_stub(temp.path(), "sleep 30", "exit 0");
    let ffprobe = fake_ffprobe(temp.path());

    let started = std::time::Instant::now();
    let error = preflight_with_vaapi(&ffmpeg, &ffprobe, &config)
        .unwrap_err()
        .to_string();

    assert!(
        error.contains("hevc_vaapi") && error.contains("exceeded"),
        "probe expiry must name the codec that did not prove: {error}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "expiry must be prompt, took {:?}",
        started.elapsed()
    );
}

/// Diagnostic 9b: the concurrent capacity probe is bounded by its own clock, and
/// its expiry is still a capacity failure rather than a codec failure.
#[cfg(unix)]
#[test]
fn vaapi_capacity_clock_expiry_names_the_declaration() {
    let temp = tempfile::tempdir().unwrap();
    let (_fake, mut config) = proven_device(temp.path());
    config.max_sessions = 2;
    config.clocks.capacity_clock = Duration::from_millis(150);
    let counter = temp.path().join("encode-count");
    let ffmpeg = vaapi_ffmpeg_stub(
        temp.path(),
        &format!(
            "printf x >> {counter}; if [ \"$(wc -c < {counter})\" -gt 1 ]; then sleep 30; fi; \
             {ENCODE_OK}",
            counter = counter.display()
        ),
        "exit 0",
    );
    let ffprobe = fake_ffprobe(temp.path());

    let error = preflight_with_vaapi(&ffmpeg, &ffprobe, &config)
        .unwrap_err()
        .to_string();

    assert!(
        error.contains("VAAPI capacity probe for 2 concurrent"),
        "capacity-clock expiry stays a capacity diagnostic: {error}"
    );
}

/// Diagnostic 9c: the overall readiness deadline. ADR 0052 §7 requires expiry to
/// fail startup naming the stage that did not prove, so a worker cannot sit
/// pending forever while probes crawl.
#[cfg(unix)]
#[test]
fn vaapi_readiness_deadline_expiry_names_the_stage() {
    let temp = tempfile::tempdir().unwrap();
    let (_fake, mut config) = proven_device(temp.path());
    config.clocks.readiness_deadline = Duration::ZERO;
    let ffmpeg = vaapi_ffmpeg_stub(temp.path(), ENCODE_OK, "exit 0");
    let ffprobe = fake_ffprobe(temp.path());

    let error = preflight_with_vaapi(&ffmpeg, &ffprobe, &config)
        .unwrap_err()
        .to_string();

    assert!(
        error.contains("VAAPI readiness deadline"),
        "the overall deadline needs its own diagnostic: {error}"
    );
    assert!(
        error.contains("hevc_vaapi"),
        "expiry must name the stage that did not prove: {error}"
    );
}

/// The declaration bound is ADR 0049 §3's, adopted unchanged: it stops startup
/// spawning an unbounded number of probe encodes.
#[test]
fn vaapi_declared_capacity_is_bounded_and_defaults_to_one() {
    let config = vaapi_config_from_env_values(Some("0000:f4:00.0".to_owned()), None, None, None)
        .unwrap()
        .unwrap();
    assert_eq!(config.max_sessions, 1);
    assert_eq!(config.dri_root, Path::new("/dev/dri"));
    assert_eq!(config.drm_sysfs_root, Path::new("/sys/class/drm"));

    for sessions in ["0", "17", "not-a-number"] {
        let error = vaapi_config_from_env_values(
            Some("0000:f4:00.0".to_owned()),
            Some(sessions),
            None,
            None,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("1..=16"), "{sessions}: {error}");
    }
    assert_eq!(
        vaapi_config_from_env_values(Some("0000:f4:00.0".to_owned()), Some("16"), None, None)
            .unwrap()
            .unwrap()
            .max_sessions,
        16
    );
}

/// No configured device means no VAAPI descriptor, so software and NVIDIA workers
/// are untouched by this seam existing.
#[test]
fn no_vaapi_device_configured_yields_no_vaapi_preflight() {
    assert!(
        vaapi_config_from_env_values(None, None, None, None)
            .unwrap()
            .is_none()
    );
    assert!(
        vaapi_config_from_env_values(None, Some("4"), None, None)
            .unwrap_err()
            .to_string()
            .contains("requires")
    );
}

const ALL_ENCODERS: &str = concat!(
    "Encoders:\n",
    " V..... libx265 H.265 / HEVC\n",
    " V..... libsvtav1 SVT-AV1\n",
    " V..... libaom-av1 libaom AV1\n",
    " A..... aac AAC\n",
    " A..... libopus Opus\n",
);
const ALL_MUXERS: &str = "Muxers:\n E matroska Matroska\n E mp4 MP4\n E ogg Ogg\n";

fn fake_ffprobe(dir: &Path) -> PathBuf {
    stub_bin(dir, "ffprobe", "#!/bin/sh\necho 'ffprobe version 7.0'\n")
}

fn stub_bin(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, body).unwrap();
    make_executable(&path);
    path
}

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = std::fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions).unwrap();
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) {}
