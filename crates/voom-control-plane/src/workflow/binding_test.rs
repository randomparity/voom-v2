use super::binding::{branch_context_with_probe_codec, render_default_payload};
use super::timing::EffectiveTiming;
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
