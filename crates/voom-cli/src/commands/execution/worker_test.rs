use std::net::SocketAddr;

use voom_control_plane::{LocalWorkerHandle, LocalWorkerKind};
use voom_core::WorkerId;

use crate::cli::{LocalWorkerKindArg, WorkerKindArg, WorkerStatusArg};
use crate::commands::execution::worker::ready_line_json;

#[test]
fn worker_kind_arg_maps_to_store_vocab() {
    assert_eq!(WorkerKindArg::Local.to_store().as_str(), "local");
    assert_eq!(WorkerKindArg::Remote.to_store().as_str(), "remote");
    assert_eq!(WorkerKindArg::Synthetic.to_store().as_str(), "synthetic");
}

#[test]
fn worker_status_arg_maps_to_store_vocab() {
    assert_eq!(
        WorkerStatusArg::Registered.to_store().as_str(),
        "registered"
    );
    assert_eq!(WorkerStatusArg::Active.to_store().as_str(), "active");
    assert_eq!(WorkerStatusArg::Stale.to_store().as_str(), "stale");
    assert_eq!(WorkerStatusArg::Retired.to_store().as_str(), "retired");
}

#[test]
fn local_worker_kind_arg_maps_to_control_plane() {
    assert_eq!(
        LocalWorkerKindArg::Ffmpeg.to_control_plane(),
        LocalWorkerKind::Ffmpeg
    );
    assert_eq!(
        LocalWorkerKindArg::Mkvtoolnix.to_control_plane(),
        LocalWorkerKind::Mkvtoolnix
    );
}

#[test]
fn ready_line_json_emits_ready_signal_shape() {
    let endpoint: SocketAddr = "127.0.0.1:54321".parse().unwrap();
    let handle = LocalWorkerHandle {
        worker_id: WorkerId(7),
        kind: LocalWorkerKind::Ffmpeg,
        endpoint,
    };

    let line = ready_line_json(&handle).unwrap();
    let value: serde_json::Value = serde_json::from_str(&line).unwrap();

    assert_eq!(value["status"], "ready");
    assert_eq!(value["worker_id"], 7);
    assert_eq!(value["kind"], "ffmpeg");
    assert_eq!(value["endpoint"], "127.0.0.1:54321");
}

#[test]
fn ready_line_json_labels_mkvtoolnix_kind() {
    let endpoint: SocketAddr = "127.0.0.1:9000".parse().unwrap();
    let handle = LocalWorkerHandle {
        worker_id: WorkerId(42),
        kind: LocalWorkerKind::Mkvtoolnix,
        endpoint,
    };

    let line = ready_line_json(&handle).unwrap();
    let value: serde_json::Value = serde_json::from_str(&line).unwrap();

    assert_eq!(value["kind"], "mkvtoolnix");
    assert_eq!(value["worker_id"], 42);
}
