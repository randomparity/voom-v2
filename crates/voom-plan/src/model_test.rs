use super::*;

#[test]
fn execution_plan_serializes_public_shape() {
    let plan = ExecutionPlan {
        schema_version: 1,
        plan_id: "plan_test".to_owned(),
        plan_hash: "blake3:test".to_owned(),
        policy: PolicyIdentity {
            slug: "container-metadata".to_owned(),
            source_hash: "abc".to_owned(),
            document_id: None,
            version_id: None,
        },
        input: InputIdentity {
            slug: Some("synthetic-compliant-baseline".to_owned()),
            source_label: Some("synthetic_compliant_baseline".to_owned()),
            input_set_id: None,
            fixture_labels: vec!["synthetic_compliant_baseline".to_owned()],
        },
        generated_at: None,
        summary: PlanSummary::default(),
        nodes: Vec::new(),
        edges: Vec::new(),
        warnings: Vec::new(),
        diagnostics: Vec::new(),
        provenance: PlanProvenance::default(),
    };

    let json = serde_json::to_value(&plan).unwrap();
    assert_eq!(json["schema_version"], 1);
    assert_eq!(json["plan_id"], "plan_test");
    assert_eq!(json["plan_hash"], "blake3:test");
    assert_eq!(json["policy"]["slug"], "container-metadata");
    assert_eq!(json["nodes"], serde_json::json!([]));
    assert_eq!(json["edges"], serde_json::json!([]));
}

#[test]
fn default_scheduling_hints_are_descriptive_placeholders() {
    let hints = SchedulingHints::default();
    assert_eq!(hints.priority_class, "normal");
    assert_eq!(hints.estimated_cpu_class, "unknown");
    assert_eq!(hints.estimated_gpu_class, "none");
    assert_eq!(hints.estimated_disk_bytes, Estimate::Unknown);
    assert_eq!(hints.estimated_network_bytes, Estimate::Unknown);
    assert_eq!(hints.expected_duration, Estimate::Unknown);
}
