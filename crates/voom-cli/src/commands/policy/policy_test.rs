use clap::Parser;
use serde_json::json;

use super::{PolicyInputCreateFromScanData, PolicyInputCreateFromScanSummary, source_kind_wire};
use crate::cli::{Cli, Command, PolicyCommand, PolicyInputCommand};

#[test]
fn policy_input_create_from_scan_command_parses() {
    let cli = Cli::try_parse_from([
        "voom",
        "policy",
        "input",
        "create-from-scan",
        "--slug",
        "scan-h264",
        "--file-version-id",
        "10",
        "--media-snapshot-id",
        "11",
        "--container",
        "mp4",
        "--video-codec",
        "h264",
    ])
    .unwrap();

    let parsed = match cli.command {
        Command::Policy(PolicyCommand::Input(PolicyInputCommand::CreateFromScan {
            slug,
            file_version_id,
            media_snapshot_id,
            container,
            video_codec,
        })) => Some((
            slug,
            file_version_id,
            media_snapshot_id,
            container,
            video_codec,
        )),
        _ => None,
    };
    assert!(parsed.is_some());
    let Some((slug, file_version_id, media_snapshot_id, container, video_codec)) = parsed else {
        return;
    };
    assert_eq!(slug, "scan-h264");
    assert_eq!(file_version_id, 10);
    assert_eq!(media_snapshot_id, 11);
    assert_eq!(container, "mp4");
    assert_eq!(video_codec, "h264");
}

#[test]
fn policy_input_create_from_scan_data_serializes_public_shape() {
    let data = PolicyInputCreateFromScanData {
        input_set: PolicyInputCreateFromScanSummary {
            input_set_id: 12,
            slug: "scan-h264".to_owned(),
            source_kind: "imported".to_owned(),
            file_version_id: 10,
            media_snapshot_id: 11,
        },
    };

    assert_eq!(
        serde_json::to_value(data).unwrap(),
        json!({
            "input_set": {
                "input_set_id": 12,
                "slug": "scan-h264",
                "source_kind": "imported",
                "file_version_id": 10,
                "media_snapshot_id": 11
            }
        })
    );
}

#[test]
fn policy_input_source_kind_uses_policy_wire_value() {
    assert_eq!(
        source_kind_wire(voom_policy::PolicyInputSourceKind::Imported),
        "imported"
    );
}
