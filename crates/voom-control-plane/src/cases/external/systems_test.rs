use serde_json::json;
use voom_core::ExternalSystemId;
use voom_events::EventKind;
use voom_store::repo::external::links::{ExternalLinkTargetType, NewExternalLink};
use voom_store::repo::external::path_mappings::{
    NewExternalPathMapping, PathMappingUpdate, PathVisibility,
};
use voom_store::repo::external::systems::{
    ExternalSystemHealth, ExternalSystemKind, NewExternalSystem,
};

use crate::ControlPlane;
use crate::cases::{count, cp};

fn sample(kind: ExternalSystemKind) -> NewExternalSystem {
    NewExternalSystem {
        kind,
        display_name: "media".to_owned(),
        connection_profile: json!({}),
        auth_ref: "none".to_owned(),
        rate_limit_config: json!({}),
    }
}

async fn mapping(cp: &ControlPlane, system_id: ExternalSystemId, external_prefix: &str) {
    cp.create_external_path_mapping(NewExternalPathMapping {
        external_system_id: system_id,
        internal_prefix: "/srv/media".to_owned(),
        external_prefix: external_prefix.to_owned(),
        visibility: PathVisibility::ReadOnly,
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn register_emits_registered_event_and_persists() {
    let (cp, _tmp) = cp().await;
    let system = cp
        .register_external_system(sample(ExternalSystemKind::Filesystem))
        .await
        .unwrap();
    assert_eq!(system.health_status, ExternalSystemHealth::Unknown);
    assert_eq!(count(&cp, EventKind::ExternalSystemRegistered).await, 1);
    let listed = cp.list_external_systems().await.unwrap();
    assert_eq!(listed, vec![system]);
}

#[tokio::test]
async fn health_check_reports_healthy_when_all_prefixes_exist() {
    let (cp, _tmp) = cp().await;
    let dir = tempfile::tempdir().unwrap();
    let system = cp
        .register_external_system(sample(ExternalSystemKind::Filesystem))
        .await
        .unwrap();
    mapping(&cp, system.id, dir.path().to_str().unwrap()).await;

    let checked = cp.health_check_external_system(system.id).await.unwrap();
    assert_eq!(checked.health_status, ExternalSystemHealth::Healthy);
    assert_eq!(count(&cp, EventKind::ExternalSystemHealthChanged).await, 1);

    // A second identical probe changes nothing and emits no new event.
    let again = cp.health_check_external_system(system.id).await.unwrap();
    assert_eq!(again.health_status, ExternalSystemHealth::Healthy);
    assert_eq!(count(&cp, EventKind::ExternalSystemHealthChanged).await, 1);
}

#[tokio::test]
async fn health_check_degrades_and_unreaches_by_prefix_presence() {
    let (cp, _tmp) = cp().await;
    let dir = tempfile::tempdir().unwrap();
    let system = cp
        .register_external_system(sample(ExternalSystemKind::Filesystem))
        .await
        .unwrap();
    mapping(&cp, system.id, dir.path().to_str().unwrap()).await;
    mapping(&cp, system.id, "/voom/does/not/exist").await;
    let checked = cp.health_check_external_system(system.id).await.unwrap();
    assert_eq!(checked.health_status, ExternalSystemHealth::Degraded);

    // Drop the only present prefix -> unreachable.
    drop(dir);
    let checked = cp.health_check_external_system(system.id).await.unwrap();
    assert_eq!(checked.health_status, ExternalSystemHealth::Unreachable);
}

#[tokio::test]
async fn health_check_of_non_filesystem_kind_is_unknown() {
    let (cp, _tmp) = cp().await;
    let system = cp
        .register_external_system(sample(ExternalSystemKind::Plex))
        .await
        .unwrap();
    let checked = cp.health_check_external_system(system.id).await.unwrap();
    assert_eq!(checked.health_status, ExternalSystemHealth::Unknown);
    // Unknown -> Unknown is not a change, so no event.
    assert_eq!(count(&cp, EventKind::ExternalSystemHealthChanged).await, 0);
}

#[tokio::test]
async fn health_check_unknown_system_is_not_found() {
    let (cp, _tmp) = cp().await;
    let err = cp
        .health_check_external_system(ExternalSystemId(999))
        .await
        .unwrap_err();
    assert!(
        matches!(err, voom_core::VoomError::NotFound(_)),
        "got: {err:?}"
    );
}

#[tokio::test]
async fn path_mapping_crud_round_trips() {
    let (cp, _tmp) = cp().await;
    let system = cp
        .register_external_system(sample(ExternalSystemKind::Filesystem))
        .await
        .unwrap();
    let created = cp
        .create_external_path_mapping(NewExternalPathMapping {
            external_system_id: system.id,
            internal_prefix: "/srv/media".to_owned(),
            external_prefix: "/data".to_owned(),
            visibility: PathVisibility::ReadOnly,
        })
        .await
        .unwrap();
    let updated = cp
        .update_external_path_mapping(
            created.id,
            PathMappingUpdate {
                visibility: Some(PathVisibility::ReadWrite),
                ..PathMappingUpdate::default()
            },
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(updated.visibility, PathVisibility::ReadWrite);
    assert_eq!(
        cp.list_external_path_mappings(system.id)
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(cp.delete_external_path_mapping(created.id).await.unwrap());
    assert!(
        cp.list_external_path_mappings(system.id)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn link_and_unlink_emit_events() {
    let (cp, _tmp) = cp().await;
    let system = cp
        .register_external_system(sample(ExternalSystemKind::Plex))
        .await
        .unwrap();
    let link = cp
        .link_external_ref(NewExternalLink {
            external_system_id: system.id,
            target_type: ExternalLinkTargetType::MediaWork,
            target_id: 42,
            external_ref: "plex://1".to_owned(),
        })
        .await
        .unwrap();
    assert_eq!(count(&cp, EventKind::ExternalSystemLinked).await, 1);

    let unlinked = cp.unlink_external_ref(link.id).await.unwrap().unwrap();
    assert!(unlinked.retired_at.is_some());
    assert_eq!(count(&cp, EventKind::ExternalSystemUnlinked).await, 1);
    // Second unlink is a no-op (already retired), no extra event.
    assert!(cp.unlink_external_ref(link.id).await.unwrap().is_none());
    assert_eq!(count(&cp, EventKind::ExternalSystemUnlinked).await, 1);
}
