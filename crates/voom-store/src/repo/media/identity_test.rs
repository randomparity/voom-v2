use super::*;

use serde_json::json;
use time::Duration;
use voom_core::PolicyVersionId;

use crate::repo::policy::policies::{NewPolicyDocumentVersion, SqlitePolicyRepo};
use crate::repo::policy::policy_inputs::SqlitePolicyInputRepo;
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

#[tokio::test]
async fn create_file_version_accepts_staged_commit_with_parent() {
    let (repo, _tmp) = fresh().await;
    let asset = repo.create_file_asset(T0).await.unwrap();
    let source = repo
        .create_file_version(NewFileVersion {
            file_asset_id: asset.id,
            content_hash: "hash-source".to_owned(),
            size_bytes: 7,
            produced_by: ProducedBy::Ingest,
            produced_from_version_id: None,
            created_at: T0,
        })
        .await
        .unwrap();

    let committed = repo
        .create_file_version(NewFileVersion {
            file_asset_id: asset.id,
            content_hash: "hash-staged".to_owned(),
            size_bytes: 7,
            produced_by: ProducedBy::StagedCommit,
            produced_from_version_id: Some(source.id),
            created_at: T0,
        })
        .await
        .unwrap();

    let got = repo.get_file_version(committed.id).await.unwrap().unwrap();
    assert_eq!(got.produced_by, ProducedBy::StagedCommit);
    assert_eq!(got.produced_from_version_id, Some(source.id));
}

#[tokio::test]
async fn create_file_version_requires_parent_for_staged_commit() {
    let (repo, _tmp) = fresh().await;
    let asset = repo.create_file_asset(T0).await.unwrap();
    let err = repo
        .create_file_version(NewFileVersion {
            file_asset_id: asset.id,
            content_hash: "hash-staged".to_owned(),
            size_bytes: 7,
            produced_by: ProducedBy::StagedCommit,
            produced_from_version_id: None,
            created_at: T0,
        })
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::Conflict(_)), "got: {err:?}");
}

#[tokio::test]
async fn create_file_version_in_tx_rejects_null_parent_staged_commit() {
    let (repo, _tmp) = fresh().await;
    let asset = repo.create_file_asset(T0).await.unwrap();
    let mut tx = repo.pool.begin().await.unwrap();

    let err = repo
        .create_file_version_in_tx(
            &mut tx,
            NewFileVersion {
                file_asset_id: asset.id,
                content_hash: "hash-staged-in-tx".to_owned(),
                size_bytes: 7,
                produced_by: ProducedBy::StagedCommit,
                produced_from_version_id: None,
                created_at: T0,
            },
        )
        .await
        .unwrap_err();
    tx.commit().await.unwrap();

    assert!(matches!(err, VoomError::Conflict(_)), "got: {err:?}");
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
                ..AcceptedPin::default()
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
async fn accept_rejects_superseded_evidence() {
    // A superseded row leaves accepted_at NULL on the old row; the accept
    // UPDATE must not match it, so a stale UI / retried operator action
    // cannot stamp acceptance on an obsolete assertion.
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
    let _new = repo
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
                provenance: json!({}),
                observed_at: T0 + Duration::seconds(5),
            },
            T0 + Duration::seconds(5),
        )
        .await
        .unwrap();
    // Attempting to accept the now-superseded original must Conflict.
    let err = repo
        .accept_identity_evidence_in_tx(
            &mut tx,
            old.id,
            Some("operator".to_owned()),
            T0 + Duration::seconds(6),
            AcceptedPin::default(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::Conflict(_)), "got: {err:?}");
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn accept_rejects_policy_input_set_id_as_policy_version_id() {
    let (repo, _tmp) = fresh().await;
    let policy_inputs = SqlitePolicyInputRepo::new(repo.pool.clone());
    let input_set = policy_inputs
        .create_input_set(
            voom_policy::load_fixture(voom_policy::FixtureName::SyntheticCompliantBaseline)
                .unwrap(),
        )
        .await
        .unwrap();
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

    let err = repo
        .accept_identity_evidence_in_tx(
            &mut tx,
            evidence.id,
            Some("operator".to_owned()),
            T0 + Duration::seconds(1),
            AcceptedPin {
                policy_version_id: Some(PolicyVersionId(input_set.id.0)),
                ..AcceptedPin::default()
            },
        )
        .await
        .unwrap_err();

    assert_eq!(err.code(), "POLICY_VALIDATION_ERROR");
    tx.commit().await.unwrap();
    let reloaded = repo
        .get_identity_evidence(evidence.id)
        .await
        .unwrap()
        .unwrap();
    assert!(reloaded.accepted_policy_id.is_none());
    assert!(reloaded.accepted_at.is_none());
}

#[tokio::test]
async fn accept_stamps_real_policy_version_id() {
    let (repo, _tmp) = fresh().await;
    let policies = SqlitePolicyRepo::new(repo.pool.clone());
    let created = policies
        .create_document_with_version(NewPolicyDocumentVersion {
            slug: "identity-policy".to_owned(),
            display_name: None,
            source_text: "policy \"identity-policy\" { phase a {} }".to_owned(),
            created_at: T0,
        })
        .await
        .unwrap();
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
            Some("operator".to_owned()),
            T0 + Duration::seconds(1),
            AcceptedPin {
                policy_version_id: Some(created.version.id),
                ..AcceptedPin::default()
            },
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let reloaded = repo
        .get_identity_evidence(evidence.id)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(accepted.accepted_policy_id, Some(created.version.id.0));
    assert_eq!(reloaded.accepted_policy_id, Some(created.version.id.0));
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
        hash_match_evidence,
        path_rule_evidence,
        ..
    } = outcome
    else {
        panic!("expected NewFileAsset");
    };
    assert!(
        hash_match_evidence.is_none(),
        "no prior hash → no hash-match evidence"
    );
    assert!(
        path_rule_evidence.is_none(),
        "no alias proof supplied → no path_rule evidence"
    );
}

#[tokio::test]
async fn discovered_file_hash_match_stamps_evidence_against_existing_asset() {
    // Spec §8.7: a hash match writes an evidence row "against the
    // existing FileAsset referencing the new FileVersion". The row's
    // target is the *existing* asset (so the existing logical asset
    // accumulates candidates), and the candidate id is the *new*
    // FileVersion (the bytes that just arrived). Hash matches never
    // collapse identity — there are two distinct FileAssets after this
    // call.
    let (repo, _tmp) = fresh().await;
    // First file: creates asset A with version V_A under hash "h-dup".
    let mut tx = repo.pool.begin().await.unwrap();
    let first = repo
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
    let IngestOutcome::NewFileAsset {
        file_asset_id: existing_asset_id,
        ..
    } = first
    else {
        panic!("expected NewFileAsset on first discovery");
    };
    // Second file: same hash, different path. Creates a SECOND asset
    // (B) with version V_B, then writes hash_match evidence whose
    // target is asset A and whose candidate is V_B.
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
        file_asset_id: new_asset_id,
        file_version_id: new_version_id,
        hash_match_evidence,
        ..
    } = outcome
    else {
        panic!("expected NewFileAsset on second discovery");
    };
    assert_ne!(
        existing_asset_id, new_asset_id,
        "hash match must NOT collapse identity"
    );
    let ev_id = hash_match_evidence.expect("hash match should be detected");
    // Evidence is on the EXISTING asset, not the new file_version.
    let on_existing_asset = repo
        .list_identity_evidence_by_target(IdentityEvidenceTarget::FileAsset, existing_asset_id.0)
        .await
        .unwrap();
    assert_eq!(on_existing_asset.len(), 1);
    assert_eq!(on_existing_asset[0].id, ev_id);
    assert_eq!(
        on_existing_asset[0].assertion_type,
        voom_events::AssertionKind::HashMatch
    );
    assert_eq!(
        on_existing_asset[0].candidate_id,
        Some(new_version_id.0),
        "candidate is the NEW FileVersion that just arrived"
    );
    // No evidence on the new file_version (the old, wrong target).
    let on_new_version = repo
        .list_identity_evidence_by_target(IdentityEvidenceTarget::FileVersion, new_version_id.0)
        .await
        .unwrap();
    assert!(
        on_new_version.is_empty(),
        "evidence must not be on the new FileVersion"
    );
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

// ---- list_live_file_locations_by_version_in_tx (round-5 fix) -----------

#[tokio::test]
async fn list_live_file_locations_by_version_in_tx_sees_within_tx_inserts() {
    // Round-5 finding: the gate's closure walker MUST run inside the
    // gate's IMMEDIATE tx and see writes from that same tx. The
    // pool-reading `list_live_file_locations_by_version` cannot —
    // sqlx-on-SQLite isolates pool reads from open transactions. This
    // test pins the in-tx variant's load-bearing property: a row
    // inserted inside the tx is visible to the in-tx list before
    // the tx commits.
    let (repo, _tmp) = fresh().await;
    let asset = repo.create_file_asset(T0).await.unwrap();
    let version = repo
        .create_file_version(NewFileVersion {
            file_asset_id: asset.id,
            content_hash: "abc".to_owned(),
            size_bytes: 1,
            produced_by: ProducedBy::Ingest,
            produced_from_version_id: None,
            created_at: T0,
        })
        .await
        .unwrap();

    let mut tx = repo.pool.begin().await.unwrap();
    let loc = repo
        .create_file_location_in_tx(
            &mut tx,
            NewFileLocation {
                file_version_id: version.id,
                kind: FileLocationKind::LocalPath,
                value: "/srv/media/in-tx.mkv".to_owned(),
                proof: None,
                observed_at: T0,
            },
        )
        .await
        .unwrap();

    // BEFORE committing: the in-tx list sees the new row.
    let in_tx = repo
        .list_live_file_locations_by_version_in_tx(&mut tx, version.id)
        .await
        .unwrap();
    assert!(
        in_tx.contains(&loc.id),
        "in-tx list must see within-tx inserts; got {in_tx:?}"
    );

    tx.commit().await.unwrap();

    // Post-commit, both variants agree.
    let post = repo
        .list_live_file_locations_by_version(version.id)
        .await
        .unwrap();
    assert!(post.iter().any(|l| l.id == loc.id));
}

#[tokio::test]
async fn list_live_file_locations_by_version_in_tx_excludes_retired() {
    // Mirror of the pool variant's filtering invariant: retired
    // locations are excluded. Done inside an open tx to assert the
    // exclusion works against in-tx state too.
    let (repo, _tmp) = fresh().await;
    let asset = repo.create_file_asset(T0).await.unwrap();
    let version = repo
        .create_file_version(NewFileVersion {
            file_asset_id: asset.id,
            content_hash: "xyz".to_owned(),
            size_bytes: 1,
            produced_by: ProducedBy::Ingest,
            produced_from_version_id: None,
            created_at: T0,
        })
        .await
        .unwrap();

    let mut tx = repo.pool.begin().await.unwrap();
    let live = repo
        .create_file_location_in_tx(
            &mut tx,
            NewFileLocation {
                file_version_id: version.id,
                kind: FileLocationKind::LocalPath,
                value: "/srv/media/live.mkv".to_owned(),
                proof: None,
                observed_at: T0,
            },
        )
        .await
        .unwrap();
    let to_retire = repo
        .create_file_location_in_tx(
            &mut tx,
            NewFileLocation {
                file_version_id: version.id,
                kind: FileLocationKind::LocalPath,
                value: "/srv/media/retired.mkv".to_owned(),
                proof: None,
                observed_at: T0,
            },
        )
        .await
        .unwrap();
    repo.retire_file_location_in_tx(&mut tx, to_retire.id, T0 + Duration::seconds(1), 0)
        .await
        .unwrap();

    let in_tx = repo
        .list_live_file_locations_by_version_in_tx(&mut tx, version.id)
        .await
        .unwrap();
    assert!(in_tx.contains(&live.id));
    assert!(
        !in_tx.contains(&to_retire.id),
        "retired location must be excluded"
    );

    tx.commit().await.unwrap();
}

// ---- retire_file_location_in_tx (M2 method; sibling-test gap plug) ------

async fn fresh_with_one_live_location()
-> (SqliteIdentityRepo, FileLocation, tempfile::NamedTempFile) {
    let (repo, tmp) = fresh().await;
    let asset = repo.create_file_asset(T0).await.unwrap();
    let version = repo
        .create_file_version(NewFileVersion {
            file_asset_id: asset.id,
            content_hash: "deadbeef".to_owned(),
            size_bytes: 1024,
            produced_by: ProducedBy::Ingest,
            produced_from_version_id: None,
            created_at: T0,
        })
        .await
        .unwrap();
    let mut tx = repo.pool.begin().await.unwrap();
    let loc = repo
        .create_file_location_in_tx(
            &mut tx,
            NewFileLocation {
                file_version_id: version.id,
                kind: FileLocationKind::LocalPath,
                value: "/srv/media/a.mkv".to_owned(),
                proof: None,
                observed_at: T0,
            },
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    (repo, loc, tmp)
}

#[tokio::test]
async fn retire_file_location_happy_path_sets_retired_at_and_bumps_epoch() {
    let (repo, loc, _tmp) = fresh_with_one_live_location().await;
    assert!(loc.retired_at.is_none());
    assert_eq!(loc.epoch, 0);

    let mut tx = repo.pool.begin().await.unwrap();
    let retired = repo
        .retire_file_location_in_tx(&mut tx, loc.id, T0 + Duration::seconds(7), 0)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert!(retired.retired_at.is_some());
    assert_eq!(retired.epoch, 1);
}

#[tokio::test]
async fn retire_file_location_already_terminal_is_conflict() {
    let (repo, loc, _tmp) = fresh_with_one_live_location().await;

    let mut tx = repo.pool.begin().await.unwrap();
    repo.retire_file_location_in_tx(&mut tx, loc.id, T0 + Duration::seconds(1), 0)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    // Second retire must reject — the row is already terminal.
    let mut tx = repo.pool.begin().await.unwrap();
    let err = repo
        .retire_file_location_in_tx(&mut tx, loc.id, T0 + Duration::seconds(2), 1)
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::Conflict(_)), "got: {err:?}");
}

#[tokio::test]
async fn retire_file_location_stale_epoch_on_live_row_is_conflict() {
    // Phase C trip-wire: a member of `closure_authorized` has a
    // different `epoch` than the value the caller passes as
    // `expected_epoch`. The retire UPDATE matches zero rows and the
    // method returns Conflict. The row stays live — no partial write.
    let (repo, loc, _tmp) = fresh_with_one_live_location().await;
    assert_eq!(loc.epoch, 0);

    // Simulate a concurrent epoch bump (e.g. another commit-gate path
    // touched the row). Direct UPDATE, no API call, so the row remains
    // live (retired_at IS NULL) but its epoch is now 1.
    sqlx::query("UPDATE file_locations SET epoch = epoch + 1 WHERE id = ?")
        .bind(i64::try_from(loc.id.0).unwrap())
        .execute(&repo.pool)
        .await
        .unwrap();

    let mut tx = repo.pool.begin().await.unwrap();
    let err = repo
        .retire_file_location_in_tx(&mut tx, loc.id, T0 + Duration::seconds(1), 0)
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::Conflict(_)), "got: {err:?}");

    // Row stays live — no partial write.
    let still = repo.get_file_location(loc.id).await.unwrap().unwrap();
    assert!(still.retired_at.is_none());
    assert_eq!(still.epoch, 1);
}

// ---- retire_file_version_in_tx (M2 method; sibling-test gap plug) -------

async fn fresh_with_one_live_version() -> (SqliteIdentityRepo, FileVersion, tempfile::NamedTempFile)
{
    let (repo, tmp) = fresh().await;
    let asset = repo.create_file_asset(T0).await.unwrap();
    let version = repo
        .create_file_version(NewFileVersion {
            file_asset_id: asset.id,
            content_hash: "cafef00d".to_owned(),
            size_bytes: 2048,
            produced_by: ProducedBy::Ingest,
            produced_from_version_id: None,
            created_at: T0,
        })
        .await
        .unwrap();
    (repo, version, tmp)
}

#[tokio::test]
async fn retire_file_version_happy_path_sets_retired_at_and_bumps_epoch() {
    let (repo, version, _tmp) = fresh_with_one_live_version().await;
    assert!(version.retired_at.is_none());
    assert_eq!(version.epoch, 0);

    let mut tx = repo.pool.begin().await.unwrap();
    let retired = repo
        .retire_file_version_in_tx(&mut tx, version.id, T0 + Duration::seconds(3), 0)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert!(retired.retired_at.is_some());
    assert_eq!(retired.epoch, 1);
}

#[tokio::test]
async fn retire_file_version_already_terminal_is_conflict() {
    let (repo, version, _tmp) = fresh_with_one_live_version().await;

    let mut tx = repo.pool.begin().await.unwrap();
    repo.retire_file_version_in_tx(&mut tx, version.id, T0 + Duration::seconds(1), 0)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let mut tx = repo.pool.begin().await.unwrap();
    let err = repo
        .retire_file_version_in_tx(&mut tx, version.id, T0 + Duration::seconds(2), 1)
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::Conflict(_)), "got: {err:?}");
}

#[tokio::test]
async fn retire_file_version_stale_epoch_on_live_row_is_conflict() {
    // Same Phase C trip-wire as the file_locations counterpart.
    let (repo, version, _tmp) = fresh_with_one_live_version().await;
    assert_eq!(version.epoch, 0);

    sqlx::query("UPDATE file_versions SET epoch = epoch + 1 WHERE id = ?")
        .bind(i64::try_from(version.id.0).unwrap())
        .execute(&repo.pool)
        .await
        .unwrap();

    let mut tx = repo.pool.begin().await.unwrap();
    let err = repo
        .retire_file_version_in_tx(&mut tx, version.id, T0 + Duration::seconds(1), 0)
        .await
        .unwrap_err();
    assert!(matches!(err, VoomError::Conflict(_)), "got: {err:?}");

    let still = repo.get_file_version(version.id).await.unwrap().unwrap();
    assert!(still.retired_at.is_none());
    assert_eq!(still.epoch, 1);
}

// ---- replace_file_location_in_tx (new) ----------------------------------

#[tokio::test]
async fn replace_file_location_happy_path_retires_old_and_inserts_new() {
    let (repo, loc, _tmp) = fresh_with_one_live_location().await;
    assert_eq!(loc.epoch, 0);

    let mut tx = repo.pool.begin().await.unwrap();
    let new_id = repo
        .replace_file_location_in_tx(
            &mut tx,
            loc.id,
            0,
            NewFileLocation {
                file_version_id: loc.file_version_id,
                kind: FileLocationKind::LocalPath,
                value: "/srv/media/a.renamed.mkv".to_owned(),
                proof: None,
                observed_at: T0 + Duration::seconds(2),
            },
            T0 + Duration::seconds(2),
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();

    // Old row: terminal, epoch bumped.
    let old = repo.get_file_location(loc.id).await.unwrap().unwrap();
    assert!(old.retired_at.is_some());
    assert_eq!(old.epoch, 1);

    // New row: live, on the same version.
    let inserted = repo.get_file_location(new_id).await.unwrap().unwrap();
    assert!(inserted.retired_at.is_none());
    assert_eq!(inserted.file_version_id, loc.file_version_id);
    assert_eq!(inserted.value, "/srv/media/a.renamed.mkv");
}

#[tokio::test]
async fn replace_file_location_already_terminal_is_conflict_and_no_insert() {
    let (repo, loc, _tmp) = fresh_with_one_live_location().await;

    // Retire it first.
    let mut tx = repo.pool.begin().await.unwrap();
    repo.retire_file_location_in_tx(&mut tx, loc.id, T0 + Duration::seconds(1), 0)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let before: usize = repo
        .list_file_locations_by_version(loc.file_version_id)
        .await
        .unwrap()
        .len();

    let mut tx = repo.pool.begin().await.unwrap();
    let err = repo
        .replace_file_location_in_tx(
            &mut tx,
            loc.id,
            1,
            NewFileLocation {
                file_version_id: loc.file_version_id,
                kind: FileLocationKind::LocalPath,
                value: "/srv/media/should-not-land.mkv".to_owned(),
                proof: None,
                observed_at: T0 + Duration::seconds(3),
            },
            T0 + Duration::seconds(3),
        )
        .await
        .unwrap_err();
    drop(tx);
    assert!(matches!(err, VoomError::Conflict(_)), "got: {err:?}");

    // Atomicity: failed replace must not have inserted the new row.
    let after: usize = repo
        .list_file_locations_by_version(loc.file_version_id)
        .await
        .unwrap()
        .len();
    assert_eq!(after, before, "no new row inserted on Conflict");
}

#[tokio::test]
async fn replace_file_location_stale_epoch_on_live_row_is_conflict_and_no_insert() {
    let (repo, loc, _tmp) = fresh_with_one_live_location().await;

    // Concurrent epoch bump without going through the API.
    sqlx::query("UPDATE file_locations SET epoch = epoch + 1 WHERE id = ?")
        .bind(i64::try_from(loc.id.0).unwrap())
        .execute(&repo.pool)
        .await
        .unwrap();

    let before: usize = repo
        .list_file_locations_by_version(loc.file_version_id)
        .await
        .unwrap()
        .len();

    let mut tx = repo.pool.begin().await.unwrap();
    let err = repo
        .replace_file_location_in_tx(
            &mut tx,
            loc.id,
            0, // caller's snapshot — now stale
            NewFileLocation {
                file_version_id: loc.file_version_id,
                kind: FileLocationKind::LocalPath,
                value: "/srv/media/should-not-land.mkv".to_owned(),
                proof: None,
                observed_at: T0 + Duration::seconds(2),
            },
            T0 + Duration::seconds(2),
        )
        .await
        .unwrap_err();
    drop(tx);
    assert!(matches!(err, VoomError::Conflict(_)), "got: {err:?}");

    let after: usize = repo
        .list_file_locations_by_version(loc.file_version_id)
        .await
        .unwrap()
        .len();
    assert_eq!(after, before, "no new row inserted on Conflict");

    let still = repo.get_file_location(loc.id).await.unwrap().unwrap();
    assert!(
        still.retired_at.is_none(),
        "live row stays live on Conflict"
    );
}

#[tokio::test]
async fn replace_file_location_rejects_cross_version_supply() {
    // Round-6 finding #1: the cross-version invariant lives inside the
    // method, not just at the gate boundary. A caller that supplies
    // new_location.file_version_id ≠ retired.file_version_id is
    // rejected with Conflict, and the retire UPDATE never runs — the
    // old row stays live. Replaces the prior "trusts caller" pin from
    // edba8e4.
    //
    // The gate-boundary type-level invariant from round-2 still holds:
    // FileLocationProposal has no file_version_id, so Phase C can only
    // convert proposal → NewFileLocation by reading the retired row.
    // This sibling test is the defense-in-depth layer inside identity.
    let (repo, tmp) = fresh().await;
    let _tmp = tmp; // keep the temp file alive

    let asset = repo.create_file_asset(T0).await.unwrap();
    let version_a = repo
        .create_file_version(NewFileVersion {
            file_asset_id: asset.id,
            content_hash: "aaa".to_owned(),
            size_bytes: 1,
            produced_by: ProducedBy::Ingest,
            produced_from_version_id: None,
            created_at: T0,
        })
        .await
        .unwrap();
    let version_b = repo
        .create_file_version(NewFileVersion {
            file_asset_id: asset.id,
            content_hash: "bbb".to_owned(),
            size_bytes: 2,
            produced_by: ProducedBy::Ingest,
            produced_from_version_id: None,
            created_at: T0,
        })
        .await
        .unwrap();

    let mut tx = repo.pool.begin().await.unwrap();
    let loc_on_a = repo
        .create_file_location_in_tx(
            &mut tx,
            NewFileLocation {
                file_version_id: version_a.id,
                kind: FileLocationKind::LocalPath,
                value: "/srv/media/on-a.mkv".to_owned(),
                proof: None,
                observed_at: T0,
            },
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();

    // Snapshot the row count BEFORE the failed call so we can assert
    // no insert leaked through.
    let before: usize = repo
        .list_file_locations_by_version(version_a.id)
        .await
        .unwrap()
        .len();

    // Caller bug: supply version_b in NewFileLocation while retiring a
    // row that lives on version_a.
    let mut tx = repo.pool.begin().await.unwrap();
    let err = repo
        .replace_file_location_in_tx(
            &mut tx,
            loc_on_a.id,
            0,
            NewFileLocation {
                file_version_id: version_b.id, // ← mismatch
                kind: FileLocationKind::LocalPath,
                value: "/srv/media/should-not-land.mkv".to_owned(),
                proof: None,
                observed_at: T0 + Duration::seconds(1),
            },
            T0 + Duration::seconds(1),
        )
        .await
        .unwrap_err();
    drop(tx);
    assert!(matches!(err, VoomError::Conflict(_)), "got: {err:?}");

    // Old row must still be live — no partial write under version_a.
    let still = repo.get_file_location(loc_on_a.id).await.unwrap().unwrap();
    assert!(
        still.retired_at.is_none(),
        "live row stays live on mismatch"
    );
    assert_eq!(still.epoch, 0, "epoch not bumped on mismatch");

    // And no row leaked into version_a OR version_b.
    let after_a: usize = repo
        .list_file_locations_by_version(version_a.id)
        .await
        .unwrap()
        .len();
    assert_eq!(after_a, before, "no new row inserted under version_a");
    let after_b: usize = repo
        .list_file_locations_by_version(version_b.id)
        .await
        .unwrap()
        .len();
    assert_eq!(after_b, 0, "no new row inserted under version_b");
}

#[tokio::test]
async fn replace_file_location_savepoint_rolls_back_on_insert_failure() {
    // Round-6 finding #2: the retire+insert pair is wrapped in a
    // SAVEPOINT. If the INSERT fails, ROLLBACK TO restores the outer
    // tx to pre-UPDATE state, even if the caller subsequently commits
    // the outer tx (the failure mode Codex called out — a caller that
    // catches Err to record recovery state and commits would otherwise
    // persist data loss: retired old row + no replacement).
    //
    // We force the INSERT to fail deterministically via a BEFORE
    // INSERT trigger that RAISE(ABORT)s when value matches a sentinel
    // string. Real SQLite plumbing — proves the savepoint mechanism
    // without mocking sqlx internals.
    let (repo, loc, _tmp) = fresh_with_one_live_location().await;

    // Install the failure trigger.
    sqlx::query(
        "CREATE TRIGGER force_replace_insert_failure \
         BEFORE INSERT ON file_locations \
         WHEN NEW.value = '__force_failure_marker__' \
         BEGIN SELECT RAISE(ABORT, 'forced for atomicity test'); END",
    )
    .execute(&repo.pool)
    .await
    .unwrap();

    let mut tx = repo.pool.begin().await.unwrap();
    let err = repo
        .replace_file_location_in_tx(
            &mut tx,
            loc.id,
            0,
            NewFileLocation {
                file_version_id: loc.file_version_id, // matches → passes round-6 #1 pre-check
                kind: FileLocationKind::LocalPath,
                value: "__force_failure_marker__".to_owned(), // ← INSERT trips trigger
                proof: None,
                observed_at: T0 + Duration::seconds(2),
            },
            T0 + Duration::seconds(2),
        )
        .await
        .unwrap_err();
    // The load-bearing assertion (Codex round-6 finding #2): caller
    // catches the Err and commits the outer tx anyway. With a
    // SAVEPOINT, the retire is undone before the outer commit lands.
    tx.commit().await.unwrap();

    assert!(matches!(err, VoomError::Database(_)), "got: {err:?}");

    let still = repo.get_file_location(loc.id).await.unwrap().unwrap();
    assert!(
        still.retired_at.is_none(),
        "savepoint must roll back the retire on insert failure",
    );
    assert_eq!(still.epoch, 0, "epoch not bumped");

    // Clean up the trigger so it doesn't interfere with anything else
    // sharing the temp DB. (Temp DB is per-test, but be tidy.)
    sqlx::query("DROP TRIGGER force_replace_insert_failure")
        .execute(&repo.pool)
        .await
        .unwrap();
}

// ---- M3 Phase 2 commit 5: pending-commit lock retrofit on AliasAttached ----
//
// `record_discovered_file_in_tx::AliasAttached` consults the helper
// before persisting the new alias FileLocation. A live `commit_intents`
// row covering the affected FileVersion rejects the attach with
// `VoomError::Conflict`. No new `file_locations` row is inserted.
//
// `reconcile_rename_in_tx` deliberately does NOT consult the lock
// (arch spec lines 697–708; sprint spec §8.7). The rename must be
// allowed to land against an in-flight commit so external moves never
// deadlock the gate.

use crate::repo::audit::events::SqliteEventRepo;
use crate::repo::media::commit_safety_gate::{
    CommitGateContext, CommitTarget, DestructiveCommit, PrepareOutcome, prepare_destructive_commit,
};
use crate::test_support::FailingAliasResolver;

/// Seed a single live `file_location` with a `file_id_generation` proof
/// and return the prior location id + the `FileVersion` id it lives under.
async fn seed_local_proof_location(repo: &SqliteIdentityRepo) -> (FileLocationId, FileVersionId) {
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
    match outcome {
        IngestOutcome::NewFileAsset {
            file_version_id,
            file_location_id,
            ..
        } => (file_location_id, file_version_id),
        IngestOutcome::AliasAttached { .. } => {
            panic!("seed must produce a fresh NewFileAsset");
        }
    }
}

/// Land a `state = 'pending'` commit intent against `location_id`.
async fn seed_pending_intent_on_location(repo: &SqliteIdentityRepo, location_id: FileLocationId) {
    let events = SqliteEventRepo::new(repo.pool.clone());
    let resolver = FailingAliasResolver::new(std::iter::empty::<FileVersionId>());
    let outcome = prepare_destructive_commit(
        CommitGateContext {
            pool: &repo.pool,
            identity_repo: repo,
            event_repo: &events,
            alias_resolver: &resolver,
        },
        DestructiveCommit {
            target: CommitTarget::DeleteFileLocation(location_id),
            accepted_evidence_ids: Vec::new(),
            override_token: None,
        },
        T0,
    )
    .await
    .unwrap();
    match outcome {
        PrepareOutcome::Pending(_) => {}
        PrepareOutcome::Blocked { result, .. } => {
            panic!("seed_pending_intent: expected Pending, got Blocked({result:?})")
        }
    }
}

#[tokio::test]
async fn alias_attached_rejects_when_pending_commit_covers_file_version() {
    let (repo, _tmp) = fresh().await;
    let (prior_id, version_id) = seed_local_proof_location(&repo).await;
    seed_pending_intent_on_location(&repo, prior_id).await;

    let mut tx = repo.pool.begin().await.unwrap();
    let err = repo
        .record_discovered_file_in_tx(
            &mut tx,
            DiscoveredFile {
                location_kind: FileLocationKind::LocalPath,
                location_value: "/srv/alias.mkv".to_owned(),
                content_hash: "h".to_owned(),
                size_bytes: 1,
                observed_at: T0 + Duration::seconds(2),
                proof: Some(LocationProof::LocalFileIdGeneration {
                    file_id: 42,
                    generation: 1,
                }),
            },
            Some(AliasProof::LocalFileIdGeneration {
                file_id: 42,
                generation: 1,
                prior_location_id: prior_id,
            }),
        )
        .await
        .unwrap_err();
    // The tx is poisoned by the rejected mutation path; drop it to
    // release the connection before further pool reads.
    drop(tx);
    assert!(matches!(err, VoomError::Conflict(_)), "got: {err:?}");

    // No additional file_location row landed under the version.
    let live = repo
        .list_live_file_locations_by_version(version_id)
        .await
        .unwrap();
    assert_eq!(live.len(), 1, "no alias attach should have persisted");
    assert_eq!(live[0].id, prior_id);
}

#[tokio::test]
async fn alias_attached_succeeds_when_no_in_flight_commit_exists() {
    // No `commit_intents` row in 'pending'/'authorized' → the helper
    // returns None and `AliasAttached` runs to completion unchanged.
    let (repo, _tmp) = fresh().await;
    let (prior_id, version_id) = seed_local_proof_location(&repo).await;

    let mut tx = repo.pool.begin().await.unwrap();
    let outcome = repo
        .record_discovered_file_in_tx(
            &mut tx,
            DiscoveredFile {
                location_kind: FileLocationKind::LocalPath,
                location_value: "/srv/alias.mkv".to_owned(),
                content_hash: "h".to_owned(),
                size_bytes: 1,
                observed_at: T0 + Duration::seconds(2),
                proof: Some(LocationProof::LocalFileIdGeneration {
                    file_id: 42,
                    generation: 1,
                }),
            },
            Some(AliasProof::LocalFileIdGeneration {
                file_id: 42,
                generation: 1,
                prior_location_id: prior_id,
            }),
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let IngestOutcome::AliasAttached {
        file_version_id: attached_version,
        new_file_location_id,
    } = outcome
    else {
        panic!("expected AliasAttached, got {outcome:?}");
    };
    assert_eq!(attached_version, version_id);
    let live = repo
        .list_live_file_locations_by_version(version_id)
        .await
        .unwrap();
    assert_eq!(live.len(), 2, "both prior and new alias locations live");
    assert!(live.iter().any(|l| l.id == prior_id));
    assert!(live.iter().any(|l| l.id == new_file_location_id));
}

#[tokio::test]
async fn reconcile_rename_in_tx_proceeds_against_in_flight_commit() {
    // Architectural exemption (arch spec lines 697–708; sprint §8.7):
    // rename does NOT consult the pending-commit lock. A rename against
    // an in-flight commit on the same FileVersion must succeed. The
    // intent row stays exactly as it was — the rename is a separate
    // ingest-side reconciliation and the gate handles drift at
    // authorize/finalize time via closure-grew + re-anchoring.
    let (repo, _tmp) = fresh().await;
    let (prior_id, version_id) = seed_local_proof_location(&repo).await;
    seed_pending_intent_on_location(&repo, prior_id).await;

    // Verify the commit intent is durable in 'pending' so the
    // assertion below ("rename did not touch the intent") is load-bearing.
    let before_state: String = sqlx::query_scalar(
        "SELECT state FROM commit_intents WHERE id = (SELECT MAX(id) FROM commit_intents)",
    )
    .fetch_one(&repo.pool)
    .await
    .unwrap();
    assert_eq!(before_state, "pending");

    let outcome = repo
        .reconcile_rename(
            RenameProof::LocalFileIdGeneration {
                prior_location_id: prior_id,
                new_kind: FileLocationKind::LocalPath,
                new_value: "/srv/new.mkv".to_owned(),
                file_id: 42,
                generation: 1,
                prior_path_missing: true,
            },
            ObservedBytes {
                content_hash: "h".to_owned(),
                size_bytes: 1,
            },
            T0 + Duration::seconds(5),
        )
        .await
        .unwrap();
    assert_eq!(outcome.file_version_id, version_id);
    assert_eq!(outcome.retired_location_id, prior_id);

    // Intent row still 'pending'; rename did NOT mutate it. The
    // gate's authorize / finalize phases handle the resulting closure
    // delta — that's not this test's contract.
    let after_state: String = sqlx::query_scalar(
        "SELECT state FROM commit_intents WHERE id = (SELECT MAX(id) FROM commit_intents)",
    )
    .fetch_one(&repo.pool)
    .await
    .unwrap();
    assert_eq!(after_state, "pending");
}
