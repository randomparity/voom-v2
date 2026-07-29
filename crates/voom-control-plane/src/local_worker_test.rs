//! Unit coverage for the pure [`LocalWorkerKind`] mappings. The full
//! register -> spawn -> record -> retire lifecycle (which needs the bundled
//! worker binary built as a sibling) lives in
//! `tests/local_worker_lifecycle.rs`.

use super::{
    LocalWorkerKind, NvidiaLocalWorkerConfig, is_full_nvidia_uuid, validate_local_worker_config,
};
use voom_core::TicketOperation;

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
