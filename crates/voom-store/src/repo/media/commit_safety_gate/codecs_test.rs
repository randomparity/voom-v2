#![cfg_attr(
    test,
    expect(
        clippy::unwrap_used,
        reason = "tests favor unwrap over plumbing Result<()> through every assertion"
    )
)]

use super::*;

/// A valid `ReplaceFileLocation` target, mirroring the construction the
/// sibling `commit_safety_gate_test.rs` round-trip fixtures use. Built so
/// the regression tests below have a real `CommitTarget` to encode.
fn sample_replace_target() -> CommitTarget {
    CommitTarget::ReplaceFileLocation {
        retired: FileLocationId(3),
        new: FileLocationProposal {
            kind: FileLocationKind::LocalPath,
            value: "/tmp/stub".to_owned(),
            proof: None,
            observed_at: OffsetDateTime::UNIX_EPOCH,
        },
    }
}

fn replace_base_value() -> serde_json::Value {
    // Encode a real CommitTarget through the production encoder so the base JSON
    // is always valid, independent of the wire enum's internal shape.
    serde_json::to_value(commit_target_to_wire(&sample_replace_target())).unwrap()
}

#[test]
fn replace_variant_rejects_unknown_field() {
    // (1) base is valid — guards against a wrong-reason pass.
    let base = replace_base_value();
    assert!(
        serde_json::from_value::<CommitTargetWire>(base.clone()).is_ok(),
        "base replace payload must deserialize Ok",
    );
    // (2) base + unknown field is rejected.
    let mut v = base;
    v.as_object_mut()
        .unwrap()
        .insert("surprise".into(), serde_json::json!(1));
    assert!(
        serde_json::from_value::<CommitTargetWire>(v).is_err(),
        "unknown field in variant must be rejected",
    );
}

#[test]
fn commit_target_rejects_unknown_variant_name() {
    let v = serde_json::json!({ "kind": "teleport_file_location", "retired": "floc_1" });
    assert!(
        serde_json::from_value::<CommitTargetWire>(v).is_err(),
        "unknown variant name must be rejected",
    );
}
