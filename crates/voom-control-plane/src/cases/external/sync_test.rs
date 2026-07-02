use serde_json::json;
use voom_core::ExternalSystemId;
use voom_events::EventKind;
use voom_store::repo::external::path_mappings::{NewExternalPathMapping, PathVisibility};
use voom_store::repo::external::systems::{ExternalSystemKind, NewExternalSystem};

use crate::ControlPlane;
use crate::cases::{count, cp};

async fn filesystem_system(cp: &ControlPlane, external_prefix: Option<&str>) -> ExternalSystemId {
    let system = cp
        .register_external_system(NewExternalSystem {
            kind: ExternalSystemKind::Filesystem,
            display_name: "media".to_owned(),
            connection_profile: json!({}),
            auth_ref: "none".to_owned(),
            rate_limit_config: json!({}),
        })
        .await
        .unwrap();
    if let Some(prefix) = external_prefix {
        cp.create_external_path_mapping(NewExternalPathMapping {
            external_system_id: system.id,
            internal_prefix: "/srv/media".to_owned(),
            external_prefix: prefix.to_owned(),
            visibility: PathVisibility::ReadOnly,
        })
        .await
        .unwrap();
    }
    system.id
}

#[tokio::test]
async fn sync_records_ok_run_and_refreshes_health() {
    let (cp, _tmp) = cp().await;
    let dir = tempfile::tempdir().unwrap();
    let id = filesystem_system(&cp, Some(dir.path().to_str().unwrap())).await;

    let report = cp.sync_external_system(id).await.unwrap();
    assert_eq!(report.health_status, "healthy");
    assert_eq!(report.last_outcome.as_deref(), Some("ok"));
    assert_eq!(report.last_links_recorded, Some(0));
    assert_eq!(report.active_link_count, 0);
    assert_eq!(count(&cp, EventKind::ExternalSystemSynced).await, 1);
}

#[tokio::test]
async fn sync_of_unreachable_system_reports_failed() {
    let (cp, _tmp) = cp().await;
    let id = filesystem_system(&cp, Some("/voom/does/not/exist")).await;
    let report = cp.sync_external_system(id).await.unwrap();
    assert_eq!(report.health_status, "unreachable");
    assert_eq!(report.last_outcome.as_deref(), Some("failed"));
}

#[tokio::test]
async fn sync_unknown_system_is_not_found() {
    let (cp, _tmp) = cp().await;
    let err = cp
        .sync_external_system(ExternalSystemId(999))
        .await
        .unwrap_err();
    assert!(
        matches!(err, voom_core::VoomError::NotFound(_)),
        "got: {err:?}"
    );
}

#[tokio::test]
async fn sync_report_is_empty_before_first_sync_then_reflects_latest() {
    let (cp, _tmp) = cp().await;
    let dir = tempfile::tempdir().unwrap();
    let id = filesystem_system(&cp, Some(dir.path().to_str().unwrap())).await;

    let before = cp.external_sync_report(id).await.unwrap();
    assert_eq!(before.last_outcome, None);
    assert_eq!(before.last_started_at, None);
    assert_eq!(before.health_status, "unknown");

    cp.sync_external_system(id).await.unwrap();
    let after = cp.external_sync_report(id).await.unwrap();
    assert_eq!(after.last_outcome.as_deref(), Some("ok"));
    assert!(after.last_started_at.is_some());
    assert_eq!(after.health_status, "healthy");
}

#[tokio::test]
async fn sync_report_of_unknown_system_is_not_found() {
    let (cp, _tmp) = cp().await;
    let err = cp
        .external_sync_report(ExternalSystemId(999))
        .await
        .unwrap_err();
    assert!(
        matches!(err, voom_core::VoomError::NotFound(_)),
        "got: {err:?}"
    );
}
