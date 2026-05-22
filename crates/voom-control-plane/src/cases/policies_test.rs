use crate::cases::cp;

#[tokio::test]
async fn compile_policy_source_without_persisting() {
    let (cp, _tmp) = cp().await;

    let out = cp
        .compile_policy_source("policy \"p\" { phase a {} }")
        .await
        .unwrap();

    assert_eq!(out.policy.policy_name, "p");
    assert!(cp.list_policy_documents().await.unwrap().is_empty());
}

#[tokio::test]
async fn create_and_add_policy_versions() {
    let (cp, _tmp) = cp().await;

    let created = cp
        .create_policy_document("p", "policy \"p\" { phase a {} }")
        .await
        .unwrap();
    let version2 = cp
        .add_policy_version(
            created.document.id,
            "policy \"p\" { phase a {} phase b { depends_on: [a] } }",
        )
        .await
        .unwrap();

    assert_eq!(version2.version_number, 2);
    assert_eq!(
        cp.get_policy_document(created.document.id)
            .await
            .unwrap()
            .unwrap()
            .current_accepted_version_id,
        Some(version2.id)
    );
}
