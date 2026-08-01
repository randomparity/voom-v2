use std::net::SocketAddr;

use clap::Parser;
use voom_control_plane::workers::{
    LocalVideoAcceleratorConfig, LocalWorkerHandle, LocalWorkerKind, NvidiaLocalWorkerConfig,
    VaapiLocalWorkerConfig, VideoToolboxLocalWorkerConfig,
};
use voom_core::WorkerId;

use crate::cli::{Cli, Command, LocalWorkerKindArg, WorkerCommand, WorkerKindArg, WorkerStatusArg};
use crate::commands::execution::worker::{RunLocalAcceleratorArgs, ready_line_json};

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

#[test]
fn run_local_accepts_bounded_videotoolbox_capacity() {
    let cli = Cli::try_parse_from([
        "voom",
        "worker",
        "run-local",
        "--kind",
        "ffmpeg",
        "--videotoolbox",
        "--videotoolbox-max-sessions",
        "16",
    ])
    .unwrap();

    assert!(matches!(
        cli.command,
        Command::Worker(WorkerCommand::RunLocal { .. })
    ));
    if let Command::Worker(WorkerCommand::RunLocal {
        videotoolbox,
        videotoolbox_max_sessions,
        ..
    }) = cli.command
    {
        assert!(videotoolbox);
        assert_eq!(videotoolbox_max_sessions, Some(16));
    }
}

#[test]
fn run_local_rejects_conflicting_or_unbounded_videotoolbox_options() {
    for args in [
        vec![
            "voom",
            "worker",
            "run-local",
            "--kind",
            "ffmpeg",
            "--videotoolbox",
            "--nvidia-device",
            "GPU-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
        ],
        vec![
            "voom",
            "worker",
            "run-local",
            "--kind",
            "ffmpeg",
            "--videotoolbox-max-sessions",
            "2",
        ],
        vec![
            "voom",
            "worker",
            "run-local",
            "--kind",
            "ffmpeg",
            "--videotoolbox",
            "--videotoolbox-max-sessions",
            "17",
        ],
    ] {
        assert!(Cli::try_parse_from(args).is_err());
    }
}

#[test]
fn local_accelerator_config_types_valid_cli_state_and_applies_defaults() {
    fn args(
        nvidia_device: Option<&str>,
        nvidia_max_sessions: Option<u32>,
        vaapi_device: Option<&str>,
        vaapi_max_sessions: Option<u32>,
        videotoolbox: bool,
        videotoolbox_max_sessions: Option<u32>,
    ) -> RunLocalAcceleratorArgs {
        RunLocalAcceleratorArgs {
            nvidia_device: nvidia_device.map(str::to_owned),
            nvidia_max_sessions,
            vaapi_device: vaapi_device.map(str::to_owned),
            vaapi_max_sessions,
            videotoolbox,
            videotoolbox_max_sessions,
        }
    }

    assert_eq!(
        args(None, None, None, None, false, None).into_control_plane(),
        None
    );
    assert_eq!(
        args(
            Some("GPU-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"),
            Some(4),
            None,
            None,
            false,
            None,
        )
        .into_control_plane(),
        Some(LocalVideoAcceleratorConfig::Nvidia(
            NvidiaLocalWorkerConfig {
                device_uuid: "GPU-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".to_owned(),
                max_sessions: 4,
            }
        ))
    );
    assert_eq!(
        args(None, None, Some("0000:f4:00.0"), Some(2), false, None).into_control_plane(),
        Some(LocalVideoAcceleratorConfig::Vaapi(VaapiLocalWorkerConfig {
            pci_address: "0000:f4:00.0".to_owned(),
            max_sessions: 2,
        }))
    );
    assert_eq!(
        args(None, None, None, None, true, None).into_control_plane(),
        Some(LocalVideoAcceleratorConfig::VideoToolbox(
            VideoToolboxLocalWorkerConfig { max_sessions: 1 }
        ))
    );
    assert_eq!(
        args(None, None, Some("0000:f4:00.0"), None, false, None).into_control_plane(),
        Some(LocalVideoAcceleratorConfig::Vaapi(VaapiLocalWorkerConfig {
            pci_address: "0000:f4:00.0".to_owned(),
            max_sessions: 1,
        })),
        "an omitted session count defaults to one for every backend"
    );
}
