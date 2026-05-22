use crate::workflow::binding::{
    branch_context_with_probe_codec, render_default_payload, render_default_payload_with_fan_out,
};
use crate::workflow::model::WorkflowPlan;
use crate::workflow::timing::EffectiveTiming;
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

fn operation_name_value(operation: OperationKind) -> serde_json::Value {
    serde_json::to_value(operation).unwrap()
}
