//! Unit coverage for the pure [`LocalWorkerKind`] mappings. The full
//! register -> spawn -> record -> retire lifecycle (which needs the bundled
//! worker binary built as a sibling) lives in
//! `tests/local_worker_lifecycle.rs`.

use super::{
    LocalWorkerKind, NvidiaLocalWorkerConfig, bound_nvidia_accelerator, is_full_nvidia_uuid,
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

#[test]
fn nvidia_config_requires_ffmpeg_full_uuid_and_bounded_sessions() {
    let valid = NvidiaLocalWorkerConfig {
        device_uuid: "GPU-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".to_owned(),
        max_sessions: 16,
    };
    assert!(validate_local_worker_config(LocalWorkerKind::Ffmpeg, Some(&valid)).is_ok());
    assert!(is_full_nvidia_uuid(&valid.device_uuid));
    assert!(validate_local_worker_config(LocalWorkerKind::Mkvtoolnix, Some(&valid)).is_err());

    for device_uuid in ["0", "GPU-short", "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"] {
        let invalid = NvidiaLocalWorkerConfig {
            device_uuid: device_uuid.to_owned(),
            max_sessions: 1,
        };
        assert!(validate_local_worker_config(LocalWorkerKind::Ffmpeg, Some(&invalid)).is_err());
    }
    for max_sessions in [0, 17] {
        let invalid = NvidiaLocalWorkerConfig {
            max_sessions,
            ..valid.clone()
        };
        assert!(validate_local_worker_config(LocalWorkerKind::Ffmpeg, Some(&invalid)).is_err());
    }
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

/// `start_local_worker` configures NVIDIA devices only, so a worker reporting a
/// VAAPI device bound itself to hardware nobody asked for. Accepting it would
/// register a capability the scheduler cannot honor, so startup must fail.
#[test]
fn bound_accelerator_narrows_to_nvidia_and_rejects_an_unconfigured_vaapi_device() {
    assert!(bound_nvidia_accelerator(None).unwrap().is_none());

    let nvidia = VideoAcceleratorDescriptor::Nvidia(nvidia_descriptor());
    assert_eq!(
        bound_nvidia_accelerator(Some(&nvidia)).unwrap(),
        Some(&nvidia_descriptor())
    );

    let vaapi = VideoAcceleratorDescriptor::Vaapi(VaapiVideoAcceleratorDescriptor {
        pci_address: "0000:03:00.0".to_owned(),
        device_name: "AMD Radeon RX 7600".to_owned(),
        driver_version: "Mesa Gallium driver 25.1.7".to_owned(),
        encoders: vec!["hevc_vaapi".to_owned()],
        decoders: vec!["hevc".to_owned()],
        max_sessions: 2,
    });
    let error = bound_nvidia_accelerator(Some(&vaapi)).unwrap_err();
    assert!(
        error.to_string().contains("0000:03:00.0"),
        "the diagnostic must name the device the worker bound: {error}"
    );
}

/// Retyping `LocalWorkerBound.accelerator` must not disturb what the NVIDIA path
/// writes durably: the capability's `extra.accelerator` stays the untagged NVIDIA
/// descriptor that `video_hardware::historical_accelerator_descriptor` reads, the
/// hardware token stays the scheduler's match key, and `max_parallel` for
/// `transcode_video` stays the device's `max_sessions`.
#[tokio::test]
async fn nvidia_capability_records_the_untagged_descriptor_token_and_capacity() {
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
        capability.extra.get("accelerator").unwrap(),
        &serde_json::to_value(&descriptor).unwrap()
    );
    assert!(
        capability.extra["accelerator"].get("backend").is_none(),
        "the durable descriptor stays untagged so pre-#409 rows keep parsing"
    );
    assert_eq!(
        capability.hardware,
        vec!["nvidia:GPU-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".to_owned()]
    );
    assert_eq!(
        crate::video_hardware::historical_accelerator_descriptor(capability).unwrap(),
        Some(descriptor)
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
