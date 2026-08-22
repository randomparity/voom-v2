use super::{PublicationSummary, publish_session_evidence_in_tx};

use std::sync::{Arc, Mutex};

use serde_json::json;
use time::OffsetDateTime;
use voom_core::clock_test_support::ManualClock;
use voom_core::rng_test_support::FrozenRng;
use voom_core::{ProviderRelativeLocator, ScanSessionId, StorageRootId, VoomError};
use voom_core::{FileKeyFacts, ScanObservationEvidence};
use voom_store::repo::scan::sessions::ScanObservation;

const T0: OffsetDateTime = OffsetDateTime::UNIX_EPOCH;
const ROOT: StorageRootId = StorageRootId(9_000_001);
const SESSION: ScanSessionId = ScanSessionId(4242);

async fn publish(
    cp: &crate::ControlPlane,
    observations: Vec<ScanObservation>,
    session: ScanSessionId,
) -> Result<PublicationSummary, VoomError> {
    stage_session(cp, session, u64::try_from(observations.len()).unwrap_or(1)).await;
    let mut tx = cp.pool.begin().await.unwrap();
    // The publication input loader reads persisted rows, so stage them first.
    for observation in &observations {
        insert_observation(cp, session, observation).await;
    }
    let summary =
        publish_session_evidence_in_tx(cp, &mut tx, session, ROOT, T0).await?;
    tx.commit().await.unwrap();
    Ok(summary)
}

/// A minimal durable session plus its first accepted batch: publication reads
/// only `scan_observations`, but those rows carry foreign keys into both.
async fn stage_session(cp: &crate::ControlPlane, session: ScanSessionId, count: u64) {
    let count = i64::try_from(count).unwrap();
    sqlx::query(
        "INSERT INTO scan_sessions (id, storage_root_id, root_epoch, owner_node_id, status, \
         idle_timeout_seconds, progress_deadline_at, requested_at, \
         owner_incarnation_id, started_at) \
         VALUES (?, ?, 1, 9_000_001, 'running', 600, '1970-01-01T01:00:00Z', \
                 '1970-01-01T00:00:00Z', 'incarnation-publish', '1970-01-01T00:00:00Z')",
    )
    .bind(i64::try_from(session.0).unwrap())
    .bind(i64::try_from(ROOT.0).unwrap())
    .execute(cp.pool_for_test())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO scan_observation_batches (scan_session_id, sequence, previous_sequence, \
         request_hash, observation_count, accepted_at, cumulative_observation_count) \
         VALUES (?, 0, NULL, '0000000000000000000000000000000000000000000000000000000000000000', \
         ?, '1970-01-01T00:00:00Z', ?)",
    )
    .bind(i64::try_from(session.0).unwrap())
    .bind(count.max(1))
    .bind(count.max(1))
    .execute(cp.pool_for_test())
    .await
    .unwrap();
}

async fn insert_observation(
    cp: &crate::ControlPlane,
    session: ScanSessionId,
    observation: &ScanObservation,
) {
    let evidence_json = observation
        .evidence
        .as_ref()
        .map(|evidence| evidence.to_database_json().unwrap());
    sqlx::query(
        "INSERT INTO scan_observations (scan_session_id, batch_sequence, ordinal, \
         provider_relative_locator, provider_object_identity, size_bytes, modified_at, \
         stability_started_at, stability_confirmed_at, evidence_json) \
         VALUES (?, 0, ?, ?, 'dev=1;ino=2', ?, ?, ?, ?, ?)",
    )
    .bind(i64::try_from(session.0).unwrap())
    .bind(i64::try_from(next_ordinal(cp).await).unwrap())
    .bind(observation.provider_relative_locator.as_str())
    .bind(i64::try_from(observation.size_bytes).unwrap())
    .bind("1970-01-01T00:00:00Z")
    .bind("1970-01-01T00:00:00Z")
    .bind("1970-01-01T00:00:00Z")
    .bind(evidence_json)
    .execute(cp.pool_for_test())
    .await
    .unwrap();
}

async fn next_ordinal(cp: &crate::ControlPlane) -> u64 {
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM scan_observations WHERE scan_session_id = ?")
            .bind(i64::try_from(SESSION.0).unwrap())
            .fetch_one(cp.pool_for_test())
            .await
            .unwrap();
    u64::try_from(count).unwrap()
}

fn observation(locator: &str, evidence: Option<ScanObservationEvidence>) -> ScanObservation {
    ScanObservation {
        provider_relative_locator: ProviderRelativeLocator::new(locator.to_owned()).unwrap(),
        provider_object_identity: "dev=1;ino=2".to_owned(),
        size_bytes: evidence.as_ref().map_or(1, |e| e.size_bytes),
        modified_at: T0,
        stability_started_at: T0,
        stability_confirmed_at: T0,
        evidence,
    }
}

fn evidence(content_hash: &str) -> ScanObservationEvidence {
    evidence_with_inode(content_hash, None)
}

fn evidence_with_inode(
    content_hash: &str,
    file_key: Option<FileKeyFacts>,
) -> ScanObservationEvidence {
    ScanObservationEvidence {
        content_hash: content_hash.to_owned(),
        size_bytes: 123,
        modified_at: "1970-01-01T00:00:00Z".to_owned(),
        file_key,
        sidecars: Vec::new(),
        probe_snapshot: json!({
            "format": "publish-v1",
            "streams": [{ "index": 0, "codec_name": "h264", "disposition": { "default": true } }],
        }),
    }
}

fn blake3(hex: &str) -> String {
    format!("blake3:{hex}")
}

#[tokio::test]
async fn publishes_identity_and_media_snapshot_from_agreed_evidence() {
    let (cp, _tmp) = cp_with_manual_clock(T0).await;

    let summary = publish(
        &cp,
        vec![observation("library/movie.mkv", Some(evidence(&blake3(&"a".repeat(64)))))],
        SESSION,
    )
    .await
    .unwrap();

    assert_eq!(summary.published, 1);
    assert_eq!(table_count(&cp, "file_assets").await, 1);
    assert_eq!(table_count(&cp, "file_versions").await, 1);
    assert_eq!(table_count(&cp, "file_locations").await, 1);
    assert_eq!(table_count(&cp, "media_snapshots").await, 1);

    // Provenance is the session's batches, not a control-plane worker row.
    let probed_by: Option<i64> =
        sqlx::query_scalar("SELECT probed_by_worker_id FROM media_snapshots")
            .fetch_one(cp.pool_for_test())
            .await
            .unwrap();
    assert_eq!(probed_by, None);

    let stream_id: String = sqlx::query_scalar(
        "SELECT json_extract(payload, '$.streams[0].id') FROM media_snapshots",
    )
    .fetch_one(cp.pool_for_test())
    .await
    .unwrap();
    assert_eq!(stream_id, "stream-0");
}

#[tokio::test]
async fn evidence_less_observation_publishes_nothing() {
    let (cp, _tmp) = cp_with_manual_clock(T0).await;

    let summary = publish(
        &cp,
        vec![observation("library/mutated.mkv", None)],
        SESSION,
    )
    .await
    .unwrap();

    assert_eq!(summary.published, 0);
    assert_eq!(table_count(&cp, "file_assets").await, 0);
    assert_eq!(table_count(&cp, "file_locations").await, 0);
}

#[tokio::test]
async fn republishing_same_content_hits_same_address_replay() {
    let (cp, _tmp) = cp_with_manual_clock(T0).await;
    let first = publish(
        &cp,
        vec![observation("library/replay.mkv", Some(evidence(&blake3(&"b".repeat(64)))))],
        SESSION,
    )
    .await
    .unwrap();
    assert_eq!(first.published, 1);

    // A later session observing identical bytes at the same address must not
    // mint duplicate identity rows.
    let second_session = ScanSessionId(4243);
    let second = publish(
        &cp,
        vec![observation(
            "library/replay.mkv",
            Some(evidence(&blake3(&"b".repeat(64)))),
        )],
        second_session,
    )
    .await
    .unwrap();

    assert_eq!(second.published, 1);
    assert_eq!(second.hardlinked, 0);
    assert_eq!(table_count(&cp, "file_assets").await, 1);
    assert_eq!(table_count(&cp, "file_versions").await, 1);
    assert_eq!(table_count(&cp, "file_locations").await, 1);
    assert_eq!(table_count(&cp, "media_snapshots").await, 1);
}

#[tokio::test]
async fn changed_bytes_at_same_rooted_address_conflict() {
    let (cp, _tmp) = cp_with_manual_clock(T0).await;
    publish(
        &cp,
        vec![observation("library/edit.mkv", Some(evidence(&blake3(&"c".repeat(64)))))],
        SESSION,
    )
    .await
    .unwrap();

    let error = publish(
        &cp,
        vec![observation(
            "library/edit.mkv",
            Some(evidence(&blake3(&"d".repeat(64)))),
        )],
        ScanSessionId(4244),
    )
    .await
    .unwrap_err();

    let VoomError::Conflict(message) = error else {
        panic!("expected the rooted address collision to be a conflict");
    };
    assert!(
        message.contains("already records different bytes"),
        "got: {message}"
    );
    // The failed completion rolls back its own publication attempt only.
    assert_eq!(table_count(&cp, "file_assets").await, 1);
}

#[tokio::test]
async fn hardlink_pair_attaches_two_locations_to_one_asset() {
    let (cp, _tmp) = cp_with_manual_clock(T0).await;
    let key = FileKeyFacts {
        dev: 7,
        ino: 8,
        nlink: 2,
    };

    let summary = publish(
        &cp,
        vec![
            observation(
                "library/one.mkv",
                Some(evidence_with_inode(&blake3(&"e".repeat(64)), Some(key))),
            ),
            observation(
                "library/two.mkv",
                Some(evidence_with_inode(&blake3(&"e".repeat(64)), Some(key))),
            ),
        ],
        SESSION,
    )
    .await
    .unwrap();

    assert_eq!(summary.published, 2);
    assert_eq!(summary.hardlinked, 1);
    assert_eq!(table_count(&cp, "file_assets").await, 1);
    assert_eq!(table_count(&cp, "file_versions").await, 1);
    assert_eq!(table_count(&cp, "file_locations").await, 2);
    // Only the fresh ingest records a snapshot; the hardlink reuses it.
    assert_eq!(table_count(&cp, "media_snapshots").await, 1);
}

#[tokio::test]
async fn sidecar_evidence_attaches_bundle_membership() {
    let (cp, _tmp) = cp_with_manual_clock(T0).await;
    let mut primary = evidence(&blake3(&"f".repeat(64)));
    primary.sidecars.push(voom_core::ScanSidecarEvidence {
        provider_relative_locator: "library/movie.srt".to_owned(),
        role: "external_subtitle".to_owned(),
        sha256_hex: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
            .to_owned(),
        size_bytes: 42,
    });

    let summary = publish(
        &cp,
        vec![observation("library/movie.mkv", Some(primary))],
        SESSION,
    )
    .await
    .unwrap();

    assert_eq!(summary.published, 1);
    assert_eq!(table_count(&cp, "asset_bundles").await, 1);
    let members: Vec<(i64, String)> = sqlx::query_as(
        "SELECT file_asset_id, role FROM asset_bundle_members ORDER BY file_asset_id ASC",
    )
    .fetch_all(cp.pool_for_test())
    .await
    .unwrap();
    assert_eq!(members.len(), 2);
    assert_eq!(members[0].1, "primary_video");
    assert_eq!(members[1].1, "external_subtitle");
}

async fn table_count(cp: &crate::ControlPlane, table: &str) -> i64 {
    let sql = format!("SELECT COUNT(*) FROM {table}");
    sqlx::query_scalar(&sql)
        .fetch_one(cp.pool_for_test())
        .await
        .unwrap()
}

async fn cp_with_manual_clock(
    now: OffsetDateTime,
) -> (crate::ControlPlane, voom_test_support::TempDatabase) {
    let tmp = voom_test_support::TempDatabase::new().unwrap();
    let url = format!("sqlite://{}", tmp.path().display());
    voom_store::init(&url).await.unwrap();
    let pool = voom_store::connect(&url).await.unwrap();
    let clock = Arc::new(ManualClock::new(now));
    let cp = crate::ControlPlane::open_with_pool_and_rng(
        pool,
        clock,
        Arc::new(Mutex::new(FrozenRng::new(u32::MAX))),
    )
    .await
    .unwrap();
    (cp, tmp)
}
