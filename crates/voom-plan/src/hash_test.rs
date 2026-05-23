use crate::model::{ExecutionPlan, InputIdentity, PlanProvenance, PlanSummary, PolicyIdentity};

use super::*;

fn empty_plan(generated_at: bool) -> ExecutionPlan {
    ExecutionPlan {
        schema_version: 1,
        plan_id: String::new(),
        plan_hash: String::new(),
        policy: PolicyIdentity {
            slug: "container-metadata".to_owned(),
            source_hash: "source-hash".to_owned(),
            document_id: None,
            version_id: None,
        },
        input: InputIdentity {
            slug: Some("synthetic-compliant-baseline".to_owned()),
            source_label: Some("synthetic_compliant_baseline".to_owned()),
            input_set_id: None,
            fixture_labels: vec!["synthetic_compliant_baseline".to_owned()],
        },
        generated_at: generated_at
            .then(|| time::OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap()),
        summary: PlanSummary::default(),
        nodes: Vec::new(),
        edges: Vec::new(),
        warnings: Vec::new(),
        diagnostics: Vec::new(),
        provenance: PlanProvenance::default(),
    }
}

#[test]
fn plan_hash_ignores_plan_hash_plan_id_and_generated_at() {
    let mut left = empty_plan(false);
    let mut right = empty_plan(true);
    left.plan_id = "plan_old".to_owned();
    left.plan_hash = "blake3:old".to_owned();
    right.plan_id = "plan_new".to_owned();
    right.plan_hash = "blake3:new".to_owned();

    assert_eq!(plan_hash(&left).unwrap(), plan_hash(&right).unwrap());
}

#[test]
fn node_and_edge_ids_are_stable_from_components() {
    assert_eq!(
        node_id(
            "normalize",
            0,
            "set_container",
            "synthetic:media_variant:variant-1"
        ),
        node_id(
            "normalize",
            0,
            "set_container",
            "synthetic:media_variant:variant-1"
        )
    );
    assert!(node_id("normalize", 0, "set_container", "target").starts_with("node_"));
    assert!(edge_id("node_a", "node_b", "phase_depends_on").starts_with("edge_"));
}
