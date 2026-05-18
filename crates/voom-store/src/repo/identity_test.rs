use super::*;

use serde_json::json;
use time::Duration;

use crate::test_support::{T0, fresh_initialized_pool_at};

async fn fresh() -> (SqliteIdentityRepo, tempfile::NamedTempFile) {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let pool = fresh_initialized_pool_at(tmp.path()).await.unwrap();
    (SqliteIdentityRepo::new(pool), tmp)
}

// ---- media_works ---------------------------------------------------------

#[tokio::test]
async fn create_and_get_media_work() {
    let (repo, _tmp) = fresh().await;
    let mw = repo
        .create_media_work(NewMediaWork {
            kind: MediaWorkKind::Movie,
            display_title: "Solaris".to_owned(),
            provisional: true,
            created_at: T0,
        })
        .await
        .unwrap();
    assert_eq!(mw.epoch, 0);
    let got = repo.get_media_work(mw.id).await.unwrap().unwrap();
    assert_eq!(got.display_title, "Solaris");
    assert_eq!(got.kind, MediaWorkKind::Movie);
    assert!(got.provisional);
}

#[tokio::test]
async fn update_media_work_provisional_bumps_epoch_and_gate_on_stale_epoch() {
    let (repo, _tmp) = fresh().await;
    let mw = repo
        .create_media_work(NewMediaWork {
            kind: MediaWorkKind::Movie,
            display_title: "X".to_owned(),
            provisional: true,
            created_at: T0,
        })
        .await
        .unwrap();
    // Happy path: epoch 0 → 1.
    let mut tx = repo.pool.begin().await.unwrap();
    let updated = repo
        .update_media_work_provisional_in_tx(&mut tx, mw.id, false, 0)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert!(!updated.provisional);
    assert_eq!(updated.epoch, 1);
    // Stale epoch → Conflict.
    let mut tx = repo.pool.begin().await.unwrap();
    let err = repo
        .update_media_work_provisional_in_tx(&mut tx, mw.id, true, 0)
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::Conflict(_)), "got: {err:?}");
}

// ---- file_assets ---------------------------------------------------------

#[tokio::test]
async fn create_get_retire_file_asset() {
    let (repo, _tmp) = fresh().await;
    let asset = repo.create_file_asset(T0).await.unwrap();
    assert!(asset.retired_at.is_none());
    let mut tx = repo.pool.begin().await.unwrap();
    let retired = repo
        .retire_file_asset_in_tx(&mut tx, asset.id, T0 + Duration::seconds(5), 0)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert!(retired.retired_at.is_some());
}

// ---- file_versions: lineage CHECK ----------------------------------------

#[tokio::test]
async fn create_file_version_requires_parent_for_transcode() {
    let (repo, _tmp) = fresh().await;
    let asset = repo.create_file_asset(T0).await.unwrap();
    let err = repo
        .create_file_version(NewFileVersion {
            file_asset_id: asset.id,
            content_hash: "deadbeef".to_owned(),
            size_bytes: 100,
            produced_by: ProducedBy::Transcode,
            produced_from_version_id: None,
            created_at: T0,
        })
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::Conflict(_)), "got: {err:?}");
}

#[tokio::test]
async fn create_file_version_rejects_cross_asset_parent() {
    let (repo, _tmp) = fresh().await;
    let asset_a = repo.create_file_asset(T0).await.unwrap();
    let asset_b = repo.create_file_asset(T0).await.unwrap();
    let v_a = repo
        .create_file_version(NewFileVersion {
            file_asset_id: asset_a.id,
            content_hash: "h1".to_owned(),
            size_bytes: 10,
            produced_by: ProducedBy::Ingest,
            produced_from_version_id: None,
            created_at: T0,
        })
        .await
        .unwrap();
    let err = repo
        .create_file_version(NewFileVersion {
            file_asset_id: asset_b.id,
            content_hash: "h2".to_owned(),
            size_bytes: 11,
            produced_by: ProducedBy::Transcode,
            produced_from_version_id: Some(v_a.id),
            created_at: T0,
        })
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::Conflict(_)), "got: {err:?}");
}

#[tokio::test]
async fn create_file_version_accepts_ingest_with_null_parent() {
    let (repo, _tmp) = fresh().await;
    let asset = repo.create_file_asset(T0).await.unwrap();
    let v = repo
        .create_file_version(NewFileVersion {
            file_asset_id: asset.id,
            content_hash: "hash-a".to_owned(),
            size_bytes: 7,
            produced_by: ProducedBy::Ingest,
            produced_from_version_id: None,
            created_at: T0,
        })
        .await
        .unwrap();
    let got = repo.get_file_version(v.id).await.unwrap().unwrap();
    assert!(got.produced_from_version_id.is_none());
}

// ---- identity_evidence ---------------------------------------------------

#[tokio::test]
async fn record_and_get_identity_evidence() {
    let (repo, _tmp) = fresh().await;
    let asset = repo.create_file_asset(T0).await.unwrap();
    let mut tx = repo.pool.begin().await.unwrap();
    let evidence = repo
        .record_identity_evidence_in_tx(
            &mut tx,
            NewIdentityEvidence {
                target_type: IdentityEvidenceTarget::FileAsset,
                target_id: asset.id.0,
                assertion_type: voom_events::AssertionKind::PathRuleMatch,
                candidate_id: None,
                candidate_value: Some("/srv/files/foo.mkv".to_owned()),
                provider: "test".to_owned(),
                provider_version: "1".to_owned(),
                confidence: 0.5,
                provenance: json!({"reason": "test"}),
                observed_at: T0,
            },
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let got = repo
        .get_identity_evidence(evidence.id)
        .await
        .unwrap()
        .unwrap();
    assert!((got.confidence - 0.5).abs() < f64::EPSILON);
    assert!(got.accepted_at.is_none());
}

#[tokio::test]
async fn accept_then_re_accept_returns_conflict() {
    let (repo, _tmp) = fresh().await;
    let asset = repo.create_file_asset(T0).await.unwrap();
    let mut tx = repo.pool.begin().await.unwrap();
    let evidence = repo
        .record_identity_evidence_in_tx(
            &mut tx,
            NewIdentityEvidence {
                target_type: IdentityEvidenceTarget::FileAsset,
                target_id: asset.id.0,
                assertion_type: voom_events::AssertionKind::PathRuleMatch,
                candidate_id: None,
                candidate_value: None,
                provider: "test".to_owned(),
                provider_version: "1".to_owned(),
                confidence: 0.8,
                provenance: json!({}),
                observed_at: T0,
            },
        )
        .await
        .unwrap();
    let accepted = repo
        .accept_identity_evidence_in_tx(
            &mut tx,
            evidence.id,
            Some("alice".to_owned()),
            T0 + Duration::seconds(1),
            AcceptedPin {
                file_version_ids: Some(json!([])),
                hashes: None,
                locations: None,
            },
        )
        .await
        .unwrap();
    assert!(accepted.accepted_at.is_some());
    assert_eq!(accepted.accepted_user_id.as_deref(), Some("alice"));
    // Re-acceptance must Conflict (accepted_at is non-null).
    let err = repo
        .accept_identity_evidence_in_tx(
            &mut tx,
            evidence.id,
            None,
            T0 + Duration::seconds(2),
            AcceptedPin::default(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::Conflict(_)), "got: {err:?}");
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn supersede_inserts_new_row_and_marks_old() {
    let (repo, _tmp) = fresh().await;
    let asset = repo.create_file_asset(T0).await.unwrap();
    let mut tx = repo.pool.begin().await.unwrap();
    let old = repo
        .record_identity_evidence_in_tx(
            &mut tx,
            NewIdentityEvidence {
                target_type: IdentityEvidenceTarget::FileAsset,
                target_id: asset.id.0,
                assertion_type: voom_events::AssertionKind::PathRuleMatch,
                candidate_id: None,
                candidate_value: None,
                provider: "test".to_owned(),
                provider_version: "1".to_owned(),
                confidence: 0.4,
                provenance: json!({}),
                observed_at: T0,
            },
        )
        .await
        .unwrap();
    let new = repo
        .supersede_identity_evidence_in_tx(
            &mut tx,
            old.id,
            NewIdentityEvidence {
                target_type: IdentityEvidenceTarget::FileAsset,
                target_id: asset.id.0,
                assertion_type: voom_events::AssertionKind::PathRuleMatch,
                candidate_id: None,
                candidate_value: None,
                provider: "test".to_owned(),
                provider_version: "2".to_owned(),
                confidence: 0.9,
                provenance: json!({"why": "better signal"}),
                observed_at: T0 + Duration::seconds(5),
            },
            T0 + Duration::seconds(5),
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let old_after = repo.get_identity_evidence(old.id).await.unwrap().unwrap();
    assert_eq!(old_after.superseded_by_id, Some(new.id));
    assert!(old_after.superseded_at.is_some());
    let live = repo
        .list_live_identity_evidence_by_target(IdentityEvidenceTarget::FileAsset, asset.id.0)
        .await
        .unwrap();
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].id, new.id);
}

// ---- media_snapshots -----------------------------------------------------

#[tokio::test]
async fn record_and_list_media_snapshot() {
    let (repo, _tmp) = fresh().await;
    let asset = repo.create_file_asset(T0).await.unwrap();
    let v = repo
        .create_file_version(NewFileVersion {
            file_asset_id: asset.id,
            content_hash: "h".to_owned(),
            size_bytes: 1,
            produced_by: ProducedBy::Ingest,
            produced_from_version_id: None,
            created_at: T0,
        })
        .await
        .unwrap();
    let mut tx = repo.pool.begin().await.unwrap();
    let snap = repo
        .record_media_snapshot_in_tx(
            &mut tx,
            NewMediaSnapshot {
                file_version_id: v.id,
                probed_by: None,
                probed_at: T0,
                payload: json!({"streams": []}),
            },
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let list = repo.list_media_snapshots_by_version(v.id).await.unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id, snap.id);
}

// ---- record_discovered_file: NewFileAsset path ---------------------------

#[tokio::test]
async fn discovered_file_with_no_alias_proof_creates_new_asset() {
    let (repo, _tmp) = fresh().await;
    let mut tx = repo.pool.begin().await.unwrap();
    let outcome = repo
        .record_discovered_file_in_tx(
            &mut tx,
            DiscoveredFile {
                location_kind: FileLocationKind::LocalPath,
                location_value: "/srv/files/movie.mkv".to_owned(),
                content_hash: "hash-1".to_owned(),
                size_bytes: 1024,
                observed_at: T0,
                proof: None,
            },
            None,
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let IngestOutcome::NewFileAsset {
        hash_match_evidence_for,
        path_rule_evidence,
        ..
    } = outcome
    else {
        panic!("expected NewFileAsset");
    };
    assert!(
        hash_match_evidence_for.is_none(),
        "no prior hash → no hash-match evidence"
    );
    assert!(
        path_rule_evidence.is_none(),
        "no alias proof supplied → no path_rule evidence"
    );
}

#[tokio::test]
async fn discovered_file_hash_match_stamps_evidence() {
    let (repo, _tmp) = fresh().await;
    // First file: creates asset A with hash "h-dup".
    let mut tx = repo.pool.begin().await.unwrap();
    let _ = repo
        .record_discovered_file_in_tx(
            &mut tx,
            DiscoveredFile {
                location_kind: FileLocationKind::LocalPath,
                location_value: "/srv/a.mkv".to_owned(),
                content_hash: "h-dup".to_owned(),
                size_bytes: 1,
                observed_at: T0,
                proof: None,
            },
            None,
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    // Second file: same hash, different path. Must create a new asset
    // AND record hash_match evidence on the new file_version.
    let mut tx = repo.pool.begin().await.unwrap();
    let outcome = repo
        .record_discovered_file_in_tx(
            &mut tx,
            DiscoveredFile {
                location_kind: FileLocationKind::LocalPath,
                location_value: "/srv/b.mkv".to_owned(),
                content_hash: "h-dup".to_owned(),
                size_bytes: 1,
                observed_at: T0 + Duration::seconds(1),
                proof: None,
            },
            None,
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let IngestOutcome::NewFileAsset {
        file_version_id,
        hash_match_evidence_for,
        ..
    } = outcome
    else {
        panic!("expected NewFileAsset");
    };
    let other_version = hash_match_evidence_for.expect("hash match should be detected");
    let evidence = repo
        .list_identity_evidence_by_target(IdentityEvidenceTarget::FileVersion, file_version_id.0)
        .await
        .unwrap();
    assert_eq!(evidence.len(), 1);
    assert_eq!(
        evidence[0].assertion_type,
        voom_events::AssertionKind::HashMatch
    );
    assert_eq!(evidence[0].candidate_id, Some(other_version.0));
}

// ---- reconcile_rename: conflict on absent prior_path_missing flag ------

#[tokio::test]
async fn reconcile_rename_rejects_when_prior_path_not_missing() {
    let (repo, _tmp) = fresh().await;
    // Seed a location with a local proof.
    let mut tx = repo.pool.begin().await.unwrap();
    let outcome = repo
        .record_discovered_file_in_tx(
            &mut tx,
            DiscoveredFile {
                location_kind: FileLocationKind::LocalPath,
                location_value: "/srv/old.mkv".to_owned(),
                content_hash: "h".to_owned(),
                size_bytes: 1,
                observed_at: T0,
                proof: Some(LocationProof::LocalFileIdGeneration {
                    file_id: 42,
                    generation: 1,
                }),
            },
            None,
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let IngestOutcome::NewFileAsset {
        file_location_id, ..
    } = outcome
    else {
        panic!("expected NewFileAsset");
    };
    let err = repo
        .reconcile_rename(
            RenameProof::LocalFileIdGeneration {
                prior_location_id: file_location_id,
                new_kind: FileLocationKind::LocalPath,
                new_value: "/srv/new.mkv".to_owned(),
                file_id: 42,
                generation: 1,
                prior_path_missing: false,
            },
            ObservedBytes {
                content_hash: "h".to_owned(),
                size_bytes: 1,
            },
            T0 + Duration::seconds(5),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::Conflict(_)), "got: {err:?}");
    // Prior location must still be live.
    let live = repo
        .list_live_file_locations_by_version(outcome_file_version_id(&outcome))
        .await
        .unwrap();
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].id, file_location_id);
}

fn outcome_file_version_id(o: &IngestOutcome) -> FileVersionId {
    match *o {
        IngestOutcome::NewFileAsset {
            file_version_id, ..
        }
        | IngestOutcome::AliasAttached {
            file_version_id, ..
        } => file_version_id,
    }
}
