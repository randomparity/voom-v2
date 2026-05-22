use voom_policy::{FixtureName, TargetRef, load_fixture};
use voom_store::repo::policy_inputs::PolicyInputRepo;

use crate::cases::cp;

#[tokio::test]
async fn create_policy_input_set_round_trips_fixture() {
    let (cp, _tmp) = cp().await;
    let draft = load_fixture(FixtureName::SyntheticCompliantBaseline).unwrap();

    let created = cp.create_policy_input_set(draft.clone()).await.unwrap();
    let fetched = cp.get_policy_input_set(created.id).await.unwrap().unwrap();

    assert_eq!(created, fetched);
    assert_eq!(created.slug, draft.slug);
}

#[tokio::test]
async fn create_policy_input_set_rejects_invalid_model() {
    let (cp, _tmp) = cp().await;
    let mut draft = load_fixture(FixtureName::SyntheticCompliantBaseline).unwrap();
    draft.slug = " ".to_owned();

    let err = cp.create_policy_input_set(draft).await.unwrap_err();

    assert_eq!(err.code(), "POLICY_VALIDATION_ERROR");
}

#[tokio::test]
async fn list_policy_input_sets_is_deterministic() {
    let (cp, _tmp) = cp().await;
    let mut b = load_fixture(FixtureName::SyntheticNoncompliantTranscodeNeeded).unwrap();
    b.slug = "b-policy-inputs".to_owned();
    b.fixture_labels = vec!["b_policy_inputs".to_owned()];
    let mut a = load_fixture(FixtureName::SyntheticCompliantBaseline).unwrap();
    a.slug = "a-policy-inputs".to_owned();
    a.fixture_labels = vec!["a_policy_inputs".to_owned()];

    cp.create_policy_input_set(b).await.unwrap();
    cp.create_policy_input_set(a).await.unwrap();

    let listed = cp.list_policy_input_sets().await.unwrap();
    let slugs: Vec<&str> = listed.iter().map(|set| set.slug.as_str()).collect();
    assert_eq!(slugs, ["a-policy-inputs", "b-policy-inputs"]);
}

#[tokio::test]
async fn create_policy_input_set_failure_leaves_no_partial_rows() {
    let (cp, _tmp) = cp().await;
    let mut draft = load_fixture(FixtureName::SyntheticCompliantBaseline).unwrap();
    draft.media_snapshots[0].target = TargetRef::MediaWork {
        id: voom_core::MediaWorkId(9_999),
    };

    let err = cp.create_policy_input_set(draft).await.unwrap_err();
    let listed = cp.policy_inputs().list_input_sets().await.unwrap();

    assert_eq!(err.code(), "DB_UNREACHABLE");
    assert!(listed.is_empty());
}
