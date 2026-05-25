use crate::cases::cp;

use std::sync::{Arc, Mutex};

use time::OffsetDateTime;
use voom_core::{clock_test_support::FrozenClock, rng_test_support::FrozenRng};

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

#[tokio::test]
async fn create_and_add_policy_versions_use_control_plane_clock() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let url = format!("sqlite://{}", tmp.path().display());
    let _ = voom_store::init(&url).await.unwrap();
    let pool = voom_store::connect(&url).await.unwrap();
    let now = OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(42);
    let cp = crate::ControlPlane::open_with_pool_and_rng(
        pool,
        Arc::new(FrozenClock::new(now)),
        Arc::new(Mutex::new(FrozenRng::new(u32::MAX))),
    )
    .await
    .unwrap();

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

    assert_eq!(created.document.created_at, now);
    assert_eq!(created.version.created_at, now);
    assert_eq!(version2.created_at, now);
}

#[tokio::test]
async fn create_policy_document_returns_compile_diagnostics_without_persisting() {
    let (cp, _tmp) = cp().await;

    let err = cp
        .create_policy_document(
            "bad",
            "policy \"bad\" { phase a { transcode video to av1 {} } }",
        )
        .await
        .unwrap_err();

    assert_eq!(err.code(), "POLICY_VALIDATION_ERROR");
    assert!(
        err.diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code == "unsupported_transcode_shape")
    );
    assert!(cp.list_policy_documents().await.unwrap().is_empty());
}

#[tokio::test]
async fn add_policy_version_returns_compile_diagnostics_without_persisting() {
    let (cp, _tmp) = cp().await;
    let created = cp
        .create_policy_document("p", "policy \"p\" { phase a {} }")
        .await
        .unwrap();

    let err = cp
        .add_policy_version(
            created.document.id,
            "policy \"p\" { phase a { transcode video to av1 {} } }",
        )
        .await
        .unwrap_err();

    assert_eq!(err.code(), "POLICY_VALIDATION_ERROR");
    assert!(
        err.diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code == "unsupported_transcode_shape")
    );
    assert_eq!(
        cp.list_policy_versions(created.document.id)
            .await
            .unwrap()
            .len(),
        1
    );
}
