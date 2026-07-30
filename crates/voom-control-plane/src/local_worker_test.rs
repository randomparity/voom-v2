//! Unit coverage for the pure [`LocalWorkerKind`] mappings. The full
//! register -> spawn -> record -> retire lifecycle (which needs the bundled
//! worker binary built as a sibling) lives in
//! `tests/local_worker_lifecycle.rs`.

use std::time::Duration;

use super::{
    LocalVideoAcceleratorConfig, LocalWorkerKind, NvidiaLocalWorkerConfig,
    ResolvedLocalVideoAcceleratorConfig, VAAPI_STARTUP_TIMEOUT, VIDEOTOOLBOX_STARTUP_TIMEOUT,
    VaapiLocalWorkerConfig, VideoToolboxLocalWorkerConfig, is_full_nvidia_uuid,
    parse_ioreg_platform_uuid, platform_resource_id, validate_bound_accelerator,
    validate_local_worker_config,
};
#[cfg(target_os = "linux")]
use super::{kill_and_wait, process_group_has_members};
use voom_core::TicketOperation;
use voom_worker_protocol::{
    NvidiaVideoAcceleratorDescriptor, VaapiVideoAcceleratorDescriptor, VideoAcceleratorDescriptor,
};

#[test]
fn ffmpeg_maps_binary_name_and_operations() {
    assert_eq!(LocalWorkerKind::Ffmpeg.binary(), "voom-ffmpeg-worker");
    assert_eq!(LocalWorkerKind::Ffmpeg.base_name(), "local-ffmpeg");
    assert_eq!(
        LocalWorkerKind::Ffmpeg.operations(),
        &["transcode_video", "transcode_audio", "extract_audio"]
    );
}

fn nvidia_config(device_uuid: &str, max_sessions: u32) -> LocalVideoAcceleratorConfig {
    LocalVideoAcceleratorConfig::Nvidia(NvidiaLocalWorkerConfig {
        device_uuid: device_uuid.to_owned(),
        max_sessions,
    })
}

fn vaapi_config(pci_address: &str, max_sessions: u32) -> LocalVideoAcceleratorConfig {
    LocalVideoAcceleratorConfig::Vaapi(VaapiLocalWorkerConfig {
        pci_address: pci_address.to_owned(),
        max_sessions,
    })
}

/// The token is derived from the *resolved* config, because `VideoToolbox` cannot
/// produce one until it has queried the platform for its resource id.
fn resolved_nvidia(device_uuid: &str, max_sessions: u32) -> ResolvedLocalVideoAcceleratorConfig {
    ResolvedLocalVideoAcceleratorConfig::Nvidia(NvidiaLocalWorkerConfig {
        device_uuid: device_uuid.to_owned(),
        max_sessions,
    })
}

fn resolved_vaapi(pci_address: &str, max_sessions: u32) -> ResolvedLocalVideoAcceleratorConfig {
    ResolvedLocalVideoAcceleratorConfig::Vaapi(VaapiLocalWorkerConfig {
        pci_address: pci_address.to_owned(),
        max_sessions,
    })
}

#[test]
fn nvidia_config_requires_ffmpeg_full_uuid_and_bounded_sessions() {
    let valid = nvidia_config("GPU-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee", 16);
    assert!(validate_local_worker_config(LocalWorkerKind::Ffmpeg, Some(&valid)).is_ok());
    assert!(is_full_nvidia_uuid(
        "GPU-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"
    ));
    assert!(validate_local_worker_config(LocalWorkerKind::Mkvtoolnix, Some(&valid)).is_err());

    for device_uuid in ["0", "GPU-short", "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"] {
        let invalid = nvidia_config(device_uuid, 1);
        assert!(validate_local_worker_config(LocalWorkerKind::Ffmpeg, Some(&invalid)).is_err());
    }
    for max_sessions in [0, 17] {
        let invalid = nvidia_config("GPU-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee", max_sessions);
        assert!(validate_local_worker_config(LocalWorkerKind::Ffmpeg, Some(&invalid)).is_err());
    }
}

/// A VAAPI worker is configured with a PCI address, never a render-node path or
/// ordinal (ADR 0052 §1): node numbers are enumeration order and renumber, so an
/// accepted ordinal would give the worker an identity that cannot survive a
/// reboot. The session bound is ADR 0049 §3's, adopted unchanged.
#[test]
fn vaapi_config_requires_ffmpeg_a_pci_address_and_bounded_sessions() {
    let valid = vaapi_config("0000:f4:00.0", 16);
    assert!(validate_local_worker_config(LocalWorkerKind::Ffmpeg, Some(&valid)).is_ok());
    assert!(validate_local_worker_config(LocalWorkerKind::Mkvtoolnix, Some(&valid)).is_err());

    for pci_address in [
        "/dev/dri/renderD128",
        "renderD128",
        "0",
        "f4:00.0",
        "0000:F4:00.0",
    ] {
        let invalid = vaapi_config(pci_address, 1);
        let error = validate_local_worker_config(LocalWorkerKind::Ffmpeg, Some(&invalid))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("PCI address"),
            "`{pci_address}` must be rejected as not a PCI address: {error}"
        );
    }
    for max_sessions in [0, 17] {
        let invalid = vaapi_config("0000:f4:00.0", max_sessions);
        assert!(validate_local_worker_config(LocalWorkerKind::Ffmpeg, Some(&invalid)).is_err());
    }
}

/// The hardware token is the scheduler's device match key and the accelerator
/// claim's primary key, so its shape is contract. The VAAPI descriptor carries no
/// token field of its own, so it is derived here from the PCI address; keeping the
/// `<backend>:<identity>` shape means one claim table serves both backends.
#[test]
fn accelerator_hardware_tokens_are_derived_per_backend() {
    assert_eq!(
        resolved_nvidia("GPU-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee", 1).hardware_token(),
        "nvidia:GPU-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"
    );
    assert_eq!(
        resolved_vaapi("0000:f4:00.0", 1).hardware_token(),
        "vaapi:pci-0000:f4:00.0"
    );
}

/// Every party that spells the VAAPI device token must spell it identically, and
/// nothing else pins that. The supervisor writes it into the accelerator claim and the
/// capability's `hardware` column; the scheduler derives it from the stored descriptor
/// to match a candidate and to build the assignment; the worker derives it again to
/// check the assignment names the device it bound. The capacity SQL groups on that
/// exact string (`json_extract(hardware,'$[0]')`), so a one-character divergence does
/// not fail loudly — the device silently stops matching and never receives work.
#[test]
fn every_party_derives_one_identical_vaapi_device_token() {
    let pci_address = "0000:f4:00.0";

    let supervisor = resolved_vaapi(pci_address, 1).hardware_token();
    let scheduler = VideoAcceleratorDescriptor::Vaapi(vaapi_descriptor()).hardware_token();
    let worker = voom_worker_protocol::vaapi_hardware_token(pci_address);

    assert_eq!(supervisor, worker, "supervisor claim key vs worker check");
    assert_eq!(scheduler, worker, "scheduler match key vs worker check");
    assert_eq!(worker, "vaapi:pci-0000:f4:00.0");
    assert_eq!(
        vaapi_descriptor().pci_address,
        pci_address,
        "the fixtures must describe the same device for the equality to mean anything"
    );
}

fn nvidia_descriptor() -> NvidiaVideoAcceleratorDescriptor {
    NvidiaVideoAcceleratorDescriptor {
        hardware_token: "nvidia:GPU-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".to_owned(),
        device_uuid: "GPU-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".to_owned(),
        device_name: "RTX A6000".to_owned(),
        driver_version: "595.80".to_owned(),
        encoders: vec!["hevc_nvenc".to_owned()],
        decoders: vec!["h264_cuvid".to_owned(), "hevc_cuvid".to_owned()],
        max_sessions: 4,
    }
}

fn vaapi_descriptor() -> VaapiVideoAcceleratorDescriptor {
    VaapiVideoAcceleratorDescriptor {
        pci_address: "0000:f4:00.0".to_owned(),
        device_name: "AMD Radeon 8060S Graphics".to_owned(),
        driver_version: "Mesa Gallium driver 26.1.5".to_owned(),
        encoders: vec!["hevc_vaapi".to_owned()],
        decoders: vec!["h264".to_owned(), "hevc".to_owned(), "av1".to_owned()],
        max_sessions: 2,
    }
}

/// A worker that bound a device the supervisor did not configure has bound
/// hardware nobody asked for, whichever backend it is. Recording it would
/// register a capability the scheduler cannot honor against a claim the
/// supervisor does not hold, so startup must fail rather than absorb it.
#[test]
fn bound_accelerator_must_match_the_configured_device() {
    let nvidia = VideoAcceleratorDescriptor::Nvidia(nvidia_descriptor());
    let vaapi = VideoAcceleratorDescriptor::Vaapi(vaapi_descriptor());
    let nvidia_configured = resolved_nvidia("GPU-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee", 4);
    let vaapi_configured = resolved_vaapi("0000:f4:00.0", 2);

    assert!(validate_bound_accelerator(None, None).is_ok());
    assert!(validate_bound_accelerator(Some(&nvidia), Some(&nvidia_configured)).is_ok());
    assert!(validate_bound_accelerator(Some(&vaapi), Some(&vaapi_configured)).is_ok());

    let error = validate_bound_accelerator(Some(&vaapi), Some(&nvidia_configured)).unwrap_err();
    assert!(
        error.to_string().contains("0000:f4:00.0"),
        "the diagnostic must name the device the worker bound: {error}"
    );
    assert!(validate_bound_accelerator(Some(&nvidia), Some(&vaapi_configured)).is_err());
    assert!(validate_bound_accelerator(Some(&vaapi), None).is_err());
    assert!(validate_bound_accelerator(None, Some(&vaapi_configured)).is_err());

    let wrong_device = resolved_vaapi("0000:aa:00.0", 2);
    assert!(validate_bound_accelerator(Some(&vaapi), Some(&wrong_device)).is_err());
    let wrong_capacity = resolved_vaapi("0000:f4:00.0", 3);
    assert!(validate_bound_accelerator(Some(&vaapi), Some(&wrong_capacity)).is_err());
}

/// Retyping `LocalWorkerBound.accelerator` must not disturb what the NVIDIA path
/// writes durably: the capability's `extra.accelerator` stays the NVIDIA descriptor
/// that `video_hardware::historical_accelerator_descriptor` reads, the
/// hardware token stays the scheduler's match key, and `max_parallel` for
/// `transcode_video` stays the device's `max_sessions`.
#[tokio::test]
async fn nvidia_capability_records_the_tagged_descriptor_token_and_capacity() {
    let (cp, _tmp) = crate::cases::cp().await;
    let worker = cp
        .register_worker(voom_store::repo::workers::NewWorker {
            name: "accelerator-descriptor-fixture".to_owned(),
            kind: voom_core::WorkerKind::Local,
            registered_at: time::OffsetDateTime::UNIX_EPOCH,
            node_id: None,
        })
        .await
        .unwrap();
    let descriptor = nvidia_descriptor();

    cp.record_local_worker_registry(
        LocalWorkerKind::Ffmpeg,
        worker.id,
        "s3cret",
        "127.0.0.1:9000".parse().unwrap(),
        Some(&VideoAcceleratorDescriptor::Nvidia(descriptor.clone())),
    )
    .await
    .unwrap();

    let capabilities = cp
        .workers
        .operation_capability_history(&TicketOperation::new("transcode_video").unwrap())
        .await
        .unwrap();
    let capability = capabilities
        .iter()
        .find(|capability| capability.worker_id == worker.id)
        .unwrap();
    assert_eq!(
        capability.extra.get("accelerator").unwrap(),
        &serde_json::to_value(VideoAcceleratorDescriptor::Nvidia(descriptor.clone())).unwrap(),
        "the stored value is the tagged enum, not the bare NVIDIA struct"
    );
    assert_eq!(
        capability.extra["accelerator"]["backend"], "nvidia",
        "every durable descriptor carries its backend tag; migration 0031 backfilled \
         the pre-#411 rows, so the reader no longer guesses NVIDIA from an absent tag"
    );
    assert_eq!(
        capability.hardware,
        vec!["nvidia:GPU-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".to_owned()]
    );
    assert_eq!(
        crate::video_hardware::historical_accelerator_descriptor(capability).unwrap(),
        Some(VideoAcceleratorDescriptor::Nvidia(descriptor)),
        "a stored NVIDIA descriptor reads back as the NVIDIA variant"
    );

    let granted: String =
        sqlx::query_scalar("SELECT max_parallel FROM worker_grants WHERE worker_id = ?")
            .bind(i64::try_from(worker.id.0).unwrap())
            .fetch_one(&cp.pool)
            .await
            .unwrap();
    let granted: serde_json::Value = serde_json::from_str(&granted).unwrap();
    assert_eq!(granted["transcode_video"], 4);
    assert_eq!(granted["transcode_audio"], 1);

    let candidates = cp
        .workers
        .operation_candidates(&TicketOperation::new("transcode_video").unwrap())
        .await
        .unwrap();
    let candidate = candidates
        .iter()
        .find(|candidate| candidate.worker_id == worker.id)
        .unwrap();
    assert_eq!(candidate.max_parallel, 4);
}

/// The VAAPI descriptor carries no `hardware_token` field, so a stored extras
/// object cannot supply one. Per-device capacity must still resolve: a worker that
/// binds a GPU and then gets a capacity of zero never receives work, and the
/// device silently sits idle — the exact silent failure issue #409 forbids. So the
/// token comes from the capability's own `hardware` column for both backends.
///
/// The stored descriptor is `backend`-tagged for every backend: migration 0031
/// backfilled `nvidia` onto the pre-#411 rows, so a reader dispatches on the tag
/// rather than inferring NVIDIA from its absence.
#[tokio::test]
async fn vaapi_capability_records_the_tagged_descriptor_token_and_capacity() {
    let (cp, _tmp) = crate::cases::cp().await;
    let worker = cp
        .register_worker(voom_store::repo::workers::NewWorker {
            name: "vaapi-descriptor-fixture".to_owned(),
            kind: voom_core::WorkerKind::Local,
            registered_at: time::OffsetDateTime::UNIX_EPOCH,
            node_id: None,
        })
        .await
        .unwrap();
    let descriptor = VideoAcceleratorDescriptor::Vaapi(vaapi_descriptor());

    cp.record_local_worker_registry(
        LocalWorkerKind::Ffmpeg,
        worker.id,
        "s3cret",
        "127.0.0.1:9001".parse().unwrap(),
        Some(&descriptor),
    )
    .await
    .unwrap();

    let capabilities = cp
        .workers
        .operation_capability_history(&TicketOperation::new("transcode_video").unwrap())
        .await
        .unwrap();
    let capability = capabilities
        .iter()
        .find(|capability| capability.worker_id == worker.id)
        .unwrap();
    assert_eq!(
        capability.extra["accelerator"]["backend"], "vaapi",
        "a VAAPI descriptor is stored tagged so a reader can tell it from NVIDIA"
    );
    assert_eq!(
        capability.extra["accelerator"]["pci_address"],
        "0000:f4:00.0"
    );
    assert_eq!(
        capability.hardware,
        vec!["vaapi:pci-0000:f4:00.0".to_owned()]
    );

    let candidates = cp
        .workers
        .operation_candidates(&TicketOperation::new("transcode_video").unwrap())
        .await
        .unwrap();
    let candidate = candidates
        .iter()
        .find(|candidate| candidate.worker_id == worker.id)
        .unwrap();
    assert_eq!(
        candidate.max_parallel, 2,
        "a bound VAAPI worker must get the device's capacity, not zero"
    );
}

#[test]
fn videotoolbox_config_requires_ffmpeg_and_bounded_sessions() {
    for max_sessions in 1..=16 {
        let config = LocalVideoAcceleratorConfig::VideoToolbox(VideoToolboxLocalWorkerConfig {
            max_sessions,
        });
        assert!(validate_local_worker_config(LocalWorkerKind::Ffmpeg, Some(&config)).is_ok());
        assert!(validate_local_worker_config(LocalWorkerKind::Mkvtoolnix, Some(&config)).is_err());
    }
    for max_sessions in [0, 17] {
        let config = LocalVideoAcceleratorConfig::VideoToolbox(VideoToolboxLocalWorkerConfig {
            max_sessions,
        });
        assert!(validate_local_worker_config(LocalWorkerKind::Ffmpeg, Some(&config)).is_err());
    }
}

#[test]
fn videotoolbox_startup_timeout_covers_the_preflight_budget() {
    assert_eq!(VIDEOTOOLBOX_STARTUP_TIMEOUT, Duration::from_secs(465));
}

/// The supervisor must outlast the worker's own five-minute readiness deadline, or
/// it abandons the child first and the worker's stage-naming expiry (ADR 0052 §7)
/// is never observed through `voom worker run-local --vaapi-device`.
#[test]
fn vaapi_startup_timeout_outlasts_the_worker_readiness_deadline() {
    assert_eq!(VAAPI_STARTUP_TIMEOUT, Duration::from_secs(330));
}

#[test]
fn platform_uuid_is_normalized_and_hashed_without_disclosure() {
    let raw_uuid = "e4ad1c3f-8b4a-4e4e-a9ad-9a0123456789";
    let ioreg = format!(
        "    |   \"IOPlatformUUID\" = \"{raw_uuid}\"\n    |   \"manufacturer\" = <\"Apple Inc.\">"
    );

    let normalized = parse_ioreg_platform_uuid(&ioreg).unwrap();
    let resource_id = platform_resource_id(&normalized).unwrap();

    assert_eq!(normalized, raw_uuid.to_ascii_uppercase());
    assert_eq!(resource_id.len(), 64);
    assert!(
        resource_id
            .chars()
            .all(|character| character.is_ascii_hexdigit())
    );
    assert!(!resource_id.contains(raw_uuid));
}

#[test]
fn mkvtoolnix_maps_binary_name_and_operations() {
    assert_eq!(
        LocalWorkerKind::Mkvtoolnix.binary(),
        "voom-mkvtoolnix-worker"
    );
    assert_eq!(LocalWorkerKind::Mkvtoolnix.base_name(), "local-mkvtoolnix");
    assert_eq!(LocalWorkerKind::Mkvtoolnix.operations(), &["remux"]);
}

#[test]
fn every_operation_is_a_valid_ticket_operation() {
    for kind in [LocalWorkerKind::Ffmpeg, LocalWorkerKind::Mkvtoolnix] {
        for op in kind.operations() {
            assert!(
                TicketOperation::new(*op).is_ok(),
                "operation {op} must be a valid ticket operation token"
            );
        }
    }
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn failed_startup_cleanup_terminates_the_worker_process_group() {
    use std::process::Stdio;
    use std::time::Duration;

    use tokio::process::Command;

    let mut child = Command::new("sh");
    child
        .args(["-c", "sleep 30 & wait"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .kill_on_drop(true);
    let mut child = child.spawn().unwrap();
    let process_group_id = child.id().unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(process_group_has_members(process_group_id).unwrap());

    kill_and_wait(&mut child).await.unwrap();

    assert!(!process_group_has_members(process_group_id).unwrap());
}
