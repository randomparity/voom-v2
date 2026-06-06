//! Unit coverage for the pure [`LocalWorkerKind`] mappings. The full
//! register -> spawn -> record -> retire lifecycle (which needs the bundled
//! worker binary built as a sibling) lives in
//! `tests/local_worker_lifecycle.rs`.

use super::LocalWorkerKind;
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
