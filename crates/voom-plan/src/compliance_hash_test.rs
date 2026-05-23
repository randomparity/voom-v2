use serde_json::json;

use super::*;

#[test]
fn report_hash_ignores_report_hash_field() {
    let mut left = json!({"report_id": "report_a", "report_hash": "blake3:left", "checks": []});
    let mut right = left.clone();
    right["report_hash"] = json!("blake3:right");

    assert_eq!(
        report_hash_from_value(&left).unwrap(),
        report_hash_from_value(&right).unwrap()
    );

    left["checks"] = json!([{"check_id": "check_a"}]);
    assert_ne!(
        report_hash_from_value(&left).unwrap(),
        report_hash_from_value(&right).unwrap()
    );
}

#[test]
fn report_id_uses_stable_preimage() {
    let id = report_id(&json!({"plan_id": "plan_a", "checks": ["check_a"]})).unwrap();
    assert!(id.starts_with("report_"));
    assert_eq!(id.len(), "report_".len() + 16);
}
