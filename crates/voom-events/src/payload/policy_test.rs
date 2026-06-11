use super::*;
use crate::payload::Event;
use serde::Serialize;
use serde::de::DeserializeOwned;

/// Assert that `valid` round-trips and that injecting a top-level unknown field
/// is rejected by `#[serde(deny_unknown_fields)]`.
fn assert_rejects_unknown<T: Serialize + DeserializeOwned>(valid: &T) {
    let base = serde_json::to_value(valid).unwrap();
    assert!(
        serde_json::from_value::<T>(base.clone()).is_ok(),
        "base instance should deserialize: {base}"
    );
    let mut tampered = base;
    tampered
        .as_object_mut()
        .expect("payload struct serializes to a JSON object")
        .insert("__unknown".to_owned(), serde_json::json!(true));
    assert!(
        serde_json::from_value::<T>(tampered).is_err(),
        "unknown top-level field must be rejected"
    );
}

fn issue_payload(status: &str) -> IssueLifecyclePayload {
    IssueLifecyclePayload {
        issue_id: voom_core::IssueId(7),
        kind: "policy_noncompliant".to_owned(),
        status: status.to_owned(),
        dedupe_key: Some(
            "policy_noncompliant:v1:policy_document_id=1:input_set_id=2:check=a".to_owned(),
        ),
        policy_version_id: Some(voom_core::PolicyVersionId(2)),
        report_id: Some("report_abc".to_owned()),
    }
}

#[test]
fn issue_opened_payload_round_trip() {
    let p = issue_payload("planned");
    let json = serde_json::to_string(&Event::IssueOpened(p.clone())).unwrap();
    let back: Event = serde_json::from_str(&json).unwrap();
    assert_eq!(Event::IssueOpened(p), back);
}

#[test]
fn issue_updated_payload_round_trip() {
    let p = issue_payload("open");
    let json = serde_json::to_string(&Event::IssueUpdated(p.clone())).unwrap();
    let back: Event = serde_json::from_str(&json).unwrap();
    assert_eq!(Event::IssueUpdated(p), back);
}

#[test]
fn issue_resolved_payload_round_trip() {
    let p = issue_payload("resolved");
    let json = serde_json::to_string(&Event::IssueResolved(p.clone())).unwrap();
    let back: Event = serde_json::from_str(&json).unwrap();
    assert_eq!(Event::IssueResolved(p), back);
}

#[test]
fn issue_lifecycle_payload_rejects_unknown_field() {
    assert_rejects_unknown(&issue_payload("planned"));
}
