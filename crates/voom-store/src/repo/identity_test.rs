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
async fn replace_file_location_trusts_caller_supplied_version_id_by_design() {
    // The round-2 cross-version invariant ("the new location must be
    // on the same FileVersion as the retired one") is enforced at the
    // gate boundary (commit 7 / Phase C), not inside this identity
    // method. `FileLocationProposal` (the gate-level type) has no
    // `file_version_id` field, so Phase C can only source it by
    // reading the retired row's current version — meaning the only
    // way to call this method with a "wrong" version_id is to bypass
    // Phase C entirely.
    //
    // This test calls the method directly with a different version_id
    // and asserts that the method does NOT reject the call. If a
    // future change adds a defensive check inside this method, this
    // test must be updated alongside that change so the design
    // decision is re-examined explicitly, not silently weakened.
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

    // Replace under version_a's id but supply NewFileLocation with
    // version_b's id. Method does not reject by design.
    let mut tx = repo.pool.begin().await.unwrap();
    let new_id = repo
        .replace_file_location_in_tx(
            &mut tx,
            loc_on_a.id,
            0,
            NewFileLocation {
                file_version_id: version_b.id,
                kind: FileLocationKind::LocalPath,
                value: "/srv/media/now-on-b.mkv".to_owned(),
                proof: None,
                observed_at: T0 + Duration::seconds(1),
            },
            T0 + Duration::seconds(1),
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();

    // The new row landed on version_b — proving the method did not
    // re-anchor or reject.
    let inserted = repo.get_file_location(new_id).await.unwrap().unwrap();
    assert_eq!(inserted.file_version_id, version_b.id);
}
