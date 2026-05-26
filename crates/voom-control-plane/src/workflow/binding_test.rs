use crate::workflow::binding::{
    PolicyRemuxSource, branch_context_with_probe_codec, render_default_payload,
    render_default_payload_with_fan_out, render_policy_remux_payload,
};
use crate::workflow::model::WorkflowPlan;
use crate::workflow::timing::EffectiveTiming;
use voom_core::{FileLocationId, FileVersionId};
use voom_worker_protocol::OperationKind;

#[test]
fn default_payload_rendering_preserves_static_fields_then_applies_bindings() {
    let rendered = render_default_payload(
        OperationKind::ScoreQuality,
        &branch_context_with_probe_codec("file-001", "h264"),
        EffectiveTiming::for_test(25, 10),
    )
    .unwrap();
    assert_eq!(rendered["profile"], "default");
    assert_eq!(rendered["path"], "/library/file-001.mkv");
    assert_eq!(rendered["codec"], "h264");
    assert_eq!(rendered["duration_ms"], 25);
}

#[test]
fn default_payload_rendering_covers_default_ci_operations() {
    let branch = branch_context_with_probe_codec("file-001", "h264");
    let timing = EffectiveTiming::for_test(25, 10);
    for node in WorkflowPlan::default_ci().nodes {
        let payload = render_default_payload(node.operation(), &branch, timing).unwrap();
        assert_eq!(payload["operation"], operation_name_value(node.operation()));
        match node.operation() {
            OperationKind::CommitArtifact => {
                assert_eq!(payload["reason"], "quality_regression");
            }
            OperationKind::SyncExternalSystem => {
                assert_eq!(payload["system"], "plex");
                assert_eq!(payload["action"], "refresh");
            }
            OperationKind::EditTracks => {
                assert_eq!(payload["holder"], "manual");
                assert_eq!(payload["reason"], "playback");
            }
            OperationKind::ScanLibrary => {
                assert_eq!(payload["fan_out_count"], 3);
            }
            _ => {}
        }
    }
}

#[test]
fn scan_payload_uses_effective_fan_out() {
    let rendered = render_default_payload_with_fan_out(
        OperationKind::ScanLibrary,
        &branch_context_with_probe_codec("file-001", "h264"),
        EffectiveTiming::for_test(25, 10),
        7,
    )
    .unwrap();

    assert_eq!(rendered["fan_out_count"], 7);
}

#[test]
fn policy_remux_payload_renders_source_target_and_operation_payload() {
    let operation_payload = serde_json::json!({
        "type": "remux",
        "container": "mkv",
        "track_actions": [],
        "track_order": ["video", "audio", "subtitle"],
        "defaults": []
    });

    let rendered = render_policy_remux_payload(
        PolicyRemuxSource {
            file_version_id: FileVersionId(42),
            location_id: Some(FileLocationId(7)),
        },
        &operation_payload,
        std::path::Path::new("/tmp/voom-stage"),
        std::path::Path::new("/library/remux"),
        EffectiveTiming::for_test(25, 10),
    )
    .unwrap();

    assert_eq!(rendered["operation"], "remux");
    assert_eq!(rendered["remux"], operation_payload);
    assert_eq!(rendered["staging_root"], "/tmp/voom-stage");
    assert_eq!(rendered["target_dir"], "/library/remux");
    assert_eq!(rendered["duration_ms"], 25);
    assert_eq!(rendered["progress_interval_ms"], 10);
    assert_eq!(rendered["source_file_version_id"], 42);
    assert_eq!(rendered["source_location_id"], 7);
}

#[test]
fn policy_remux_payload_omits_absent_source_location() {
    let rendered = render_policy_remux_payload(
        PolicyRemuxSource {
            file_version_id: FileVersionId(42),
            location_id: None,
        },
        &serde_json::json!({
            "type": "remux",
            "container": "mkv",
            "track_actions": [],
            "track_order": ["video", "audio", "subtitle"],
            "defaults": []
        }),
        std::path::Path::new("/tmp/voom-stage"),
        std::path::Path::new("/library/remux"),
        EffectiveTiming::for_test(25, 10),
    )
    .unwrap();

    assert!(rendered.get("source_location_id").is_none());
}

#[test]
fn policy_remux_payload_rejects_non_remux_payload() {
    let err = render_policy_remux_payload(
        PolicyRemuxSource {
            file_version_id: FileVersionId(42),
            location_id: None,
        },
        &serde_json::json!({"type": "set_container", "container": "mkv"}),
        std::path::Path::new("/tmp/voom-stage"),
        std::path::Path::new("/library/remux"),
        EffectiveTiming::for_test(25, 10),
    )
    .unwrap_err();

    assert_eq!(err.to_string(), "remux payload missing `type: remux`");
}

#[test]
fn policy_remux_payload_rejects_incomplete_typed_payload() {
    let err = render_policy_remux_payload(
        PolicyRemuxSource {
            file_version_id: FileVersionId(42),
            location_id: None,
        },
        &serde_json::json!({"type": "remux"}),
        std::path::Path::new("/tmp/voom-stage"),
        std::path::Path::new("/library/remux"),
        EffectiveTiming::for_test(25, 10),
    )
    .unwrap_err();

    assert_eq!(err.to_string(), "remux payload missing `container`");
}

#[test]
fn policy_remux_payload_rejects_malformed_track_action_entry() {
    let err = render_policy_remux_payload(
        PolicyRemuxSource {
            file_version_id: FileVersionId(42),
            location_id: None,
        },
        &serde_json::json!({
            "type": "remux",
            "container": "mkv",
            "track_actions": [{"type": "keep_tracks"}],
            "track_order": ["video", "audio", "subtitle"],
            "defaults": []
        }),
        std::path::Path::new("/tmp/voom-stage"),
        std::path::Path::new("/library/remux"),
        EffectiveTiming::for_test(25, 10),
    )
    .unwrap_err();

    assert_eq!(err.to_string(), "remux track_actions[0] missing `target`");
}

#[test]
fn policy_remux_payload_rejects_malformed_track_order_entry() {
    let err = render_policy_remux_payload(
        PolicyRemuxSource {
            file_version_id: FileVersionId(42),
            location_id: None,
        },
        &serde_json::json!({
            "type": "remux",
            "container": "mkv",
            "track_actions": [],
            "track_order": ["video", 42, "subtitle"],
            "defaults": []
        }),
        std::path::Path::new("/tmp/voom-stage"),
        std::path::Path::new("/library/remux"),
        EffectiveTiming::for_test(25, 10),
    )
    .unwrap_err();

    assert_eq!(err.to_string(), "remux track_order[1] must be a string");
}

#[test]
fn policy_remux_payload_rejects_duplicate_track_order_group() {
    let err = render_policy_remux_payload(
        PolicyRemuxSource {
            file_version_id: FileVersionId(42),
            location_id: None,
        },
        &serde_json::json!({
            "type": "remux",
            "container": "mkv",
            "track_actions": [],
            "track_order": ["video", "audio", "audio"],
            "defaults": []
        }),
        std::path::Path::new("/tmp/voom-stage"),
        std::path::Path::new("/library/remux"),
        EffectiveTiming::for_test(25, 10),
    )
    .unwrap_err();

    assert_eq!(
        err.to_string(),
        "remux track_order[2] duplicates target `audio`"
    );
}

#[test]
fn policy_remux_payload_rejects_malformed_defaults_entry() {
    let err = render_policy_remux_payload(
        PolicyRemuxSource {
            file_version_id: FileVersionId(42),
            location_id: None,
        },
        &serde_json::json!({
            "type": "remux",
            "container": "mkv",
            "track_actions": [],
            "track_order": ["video", "audio", "subtitle"],
            "defaults": [{"target": "audio"}]
        }),
        std::path::Path::new("/tmp/voom-stage"),
        std::path::Path::new("/library/remux"),
        EffectiveTiming::for_test(25, 10),
    )
    .unwrap_err();

    assert_eq!(err.to_string(), "remux defaults[0] missing `strategy`");
}

fn operation_name_value(operation: OperationKind) -> serde_json::Value {
    serde_json::to_value(operation).unwrap()
}
