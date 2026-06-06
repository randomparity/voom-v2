use std::path::PathBuf;

use clap::Parser;
use serde_json::json;

use super::{
    PolicyCreateData, PolicyDocumentWire, PolicyInputCreateFromScanData,
    PolicyInputCreateFromScanSummary, PolicyListData, PolicyShowData, PolicyVersionAddData,
    PolicyVersionWire, source_kind_wire,
};
use crate::cli::{Cli, Command, PolicyCommand, PolicyInputCommand, PolicyVersionCommand};

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

#[test]
fn policy_create_command_parses() {
    let cli = Cli::try_parse_from([
        "voom",
        "policy",
        "create",
        "--slug",
        "minimal",
        "--file",
        "/tmp/minimal.voom",
    ])
    .unwrap();

    let parsed = match cli.command {
        Command::Policy(PolicyCommand::Create { slug, file }) => Some((slug, file)),
        _ => None,
    };
    assert_eq!(
        parsed,
        Some(("minimal".to_owned(), PathBuf::from("/tmp/minimal.voom")))
    );
}

#[test]
fn policy_version_add_command_parses() {
    let cli = Cli::try_parse_from([
        "voom",
        "policy",
        "version",
        "add",
        "--document-id",
        "7",
        "--file",
        "/tmp/v2.voom",
    ])
    .unwrap();

    let parsed = match cli.command {
        Command::Policy(PolicyCommand::Version(PolicyVersionCommand::Add {
            document_id,
            file,
        })) => Some((document_id, file)),
        _ => None,
    };
    assert_eq!(parsed, Some((7, PathBuf::from("/tmp/v2.voom"))));
}

#[test]
fn policy_list_and_show_commands_parse() {
    let list = Cli::try_parse_from(["voom", "policy", "list"]).unwrap();
    assert!(matches!(list.command, Command::Policy(PolicyCommand::List)));

    let show = Cli::try_parse_from(["voom", "policy", "show", "--document-id", "3"]).unwrap();
    let document_id = match show.command {
        Command::Policy(PolicyCommand::Show { document_id }) => Some(document_id),
        _ => None,
    };
    assert_eq!(document_id, Some(3));
}

#[test]
fn policy_create_data_serializes_public_shape() {
    let data = PolicyCreateData {
        document: PolicyDocumentWire {
            document_id: 1,
            slug: "minimal".to_owned(),
            display_name: "minimal".to_owned(),
            current_accepted_version_id: Some(5),
            epoch: 1,
        },
        version: PolicyVersionWire {
            version_id: 5,
            document_id: 1,
            version_number: 1,
            source_hash: "abc".to_owned(),
            schema_version: 2,
        },
    };

    assert_eq!(
        serde_json::to_value(data).unwrap(),
        json!({
            "document": {
                "document_id": 1,
                "slug": "minimal",
                "display_name": "minimal",
                "current_accepted_version_id": 5,
                "epoch": 1
            },
            "version": {
                "version_id": 5,
                "document_id": 1,
                "version_number": 1,
                "source_hash": "abc",
                "schema_version": 2
            }
        })
    );
}

#[test]
fn policy_list_and_show_and_version_add_data_serialize_public_shape() {
    let document = PolicyDocumentWire {
        document_id: 2,
        slug: "demo".to_owned(),
        display_name: "demo".to_owned(),
        current_accepted_version_id: None,
        epoch: 0,
    };
    let version = PolicyVersionWire {
        version_id: 9,
        document_id: 2,
        version_number: 3,
        source_hash: "hash".to_owned(),
        schema_version: 2,
    };

    assert_eq!(
        serde_json::to_value(PolicyListData {
            documents: vec![document],
        })
        .unwrap(),
        json!({
            "documents": [{
                "document_id": 2,
                "slug": "demo",
                "display_name": "demo",
                "current_accepted_version_id": null,
                "epoch": 0
            }]
        })
    );

    assert_eq!(
        serde_json::to_value(PolicyVersionAddData {
            version: PolicyVersionWire {
                version_id: 9,
                document_id: 2,
                version_number: 3,
                source_hash: "hash".to_owned(),
                schema_version: 2,
            },
        })
        .unwrap(),
        json!({
            "version": {
                "version_id": 9,
                "document_id": 2,
                "version_number": 3,
                "source_hash": "hash",
                "schema_version": 2
            }
        })
    );

    let show = PolicyShowData {
        document: PolicyDocumentWire {
            document_id: 2,
            slug: "demo".to_owned(),
            display_name: "demo".to_owned(),
            current_accepted_version_id: Some(9),
            epoch: 2,
        },
        versions: vec![version],
    };
    let show_json = serde_json::to_value(show).unwrap();
    assert_eq!(show_json["document"]["document_id"], 2);
    assert_eq!(show_json["versions"][0]["version_id"], 9);
}
