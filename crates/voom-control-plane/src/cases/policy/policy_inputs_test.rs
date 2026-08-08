use serde_json::json;
use time::OffsetDateTime;
use voom_core::{ErrorCode, FileVersionId, MediaSnapshotId, MediaWorkId, VoomError};
use voom_policy::{
    FixtureName, POLICY_INPUT_MAX_MEMBERS, PolicyInputSetDraft, PolicyInputSourceKind, TargetRef,
    load_fixture,
};
use voom_store::repo::audit::events::EventFilter;
use voom_store::repo::media::identity::{DiscoveredFile, IngestOutcome, NewFileLocation};
use voom_store::repo::policy::policy_inputs::PolicyInputTargetRef;
use voom_store::test_support::with_check_constraints_disabled;

use voom_store::repo::library::libraries::{LibraryMediaKind, NewLibrary};
use voom_store::repo::library::library_roots::{
    HiddenFilePolicy, LibraryScanMode, NewLibraryRoot, SymlinkPolicy,
};

use crate::cases::cp;

use super::{PolicyInputFromScanInput, RootScopedScanInput, WholeScanInput};

const T0: OffsetDateTime = OffsetDateTime::UNIX_EPOCH;

#[derive(Debug, Default, PartialEq, Eq)]
struct PolicyInputAggregateCounts {
    input_sets: i64,
    fixture_labels: i64,
    synthetic_targets: i64,
    media_snapshots: i64,
    identity_evidence: i64,
    bundle_targets: i64,
    quality_profiles: i64,
    issues: i64,
}

async fn policy_input_aggregate_counts(cp: &crate::ControlPlane) -> PolicyInputAggregateCounts {
    let row = sqlx::query_as::<_, (i64, i64, i64, i64, i64, i64, i64, i64)>(
        "SELECT \
         (SELECT COUNT(*) FROM policy_input_sets), \
         (SELECT COUNT(*) FROM policy_input_set_fixture_labels), \
         (SELECT COUNT(*) FROM policy_input_synthetic_targets), \
         (SELECT COUNT(*) FROM policy_media_snapshot_inputs), \
         (SELECT COUNT(*) FROM policy_identity_evidence_inputs), \
         (SELECT COUNT(*) FROM policy_bundle_target_inputs), \
         (SELECT COUNT(*) FROM policy_quality_profile_selections), \
         (SELECT COUNT(*) FROM policy_issue_inputs)",
    )
    .fetch_one(cp.pool_for_test())
    .await
    .unwrap();
    PolicyInputAggregateCounts {
        input_sets: row.0,
        fixture_labels: row.1,
        synthetic_targets: row.2,
        media_snapshots: row.3,
        identity_evidence: row.4,
        bundle_targets: row.5,
        quality_profiles: row.6,
        issues: row.7,
    }
}

async fn event_count(cp: &crate::ControlPlane) -> usize {
    cp.list_events(EventFilter::default(), None, 200)
        .await
        .unwrap()
        .len()
}

async fn observer_for(tmp: &voom_test_support::TempDatabase) -> crate::ControlPlane {
    let url = format!("sqlite://{}", tmp.path().display());
    let pool = voom_store::connect(&url).await.unwrap();
    crate::ControlPlane::open_with_pool(pool, std::sync::Arc::new(voom_core::SystemClock))
        .await
        .unwrap()
}

fn linked_draft(
    file_version_id: FileVersionId,
    media_snapshot_id: MediaSnapshotId,
) -> PolicyInputSetDraft {
    let mut draft = load_fixture(FixtureName::SyntheticCompliantBaseline).unwrap();
    draft.slug = format!("linked-snapshot-{}", media_snapshot_id.0);
    draft.fixture_labels = vec![format!("linked_snapshot_{}", media_snapshot_id.0)];
    draft.media_snapshots[0].target = TargetRef::FileVersion {
        id: file_version_id,
    };
    draft.media_snapshots[0].existing_media_snapshot_id = Some(media_snapshot_id);
    draft
}

fn draft_with_member_count(member_count: usize) -> PolicyInputSetDraft {
    let mut draft = load_fixture(FixtureName::SyntheticCompliantBaseline).unwrap();
    draft.slug = format!("member-budget-{member_count}");
    draft.fixture_labels = vec![format!("member_budget_{member_count}")];
    let fixed_members = draft.fixture_labels.len()
        + draft.synthetic_targets.len()
        + draft.identity_evidence.len()
        + draft.bundle_targets.len()
        + draft.quality_profiles.len()
        + draft.issues.len();
    let snapshot_count = member_count.checked_sub(fixed_members).unwrap();
    let template = draft.media_snapshots[0].clone();
    draft.media_snapshots = (0..snapshot_count)
        .map(|ordinal| voom_policy::MediaSnapshotInput {
            ordinal: u32::try_from(ordinal).unwrap(),
            ..template.clone()
        })
        .collect();
    draft
}

async fn assert_rejected_without_policy_state(
    observer: &crate::ControlPlane,
    before_events: usize,
    err: &VoomError,
    expected_code: ErrorCode,
) {
    assert_eq!(err.error_code(), expected_code);
    assert_eq!(
        policy_input_aggregate_counts(observer).await,
        PolicyInputAggregateCounts::default()
    );
    assert_eq!(event_count(observer).await, before_events);
}

#[tokio::test]
async fn generic_input_accepts_exact_snapshot_file_version_link() {
    let (cp, _tmp) = cp().await;
    let (file_version_id, media_snapshot_id) =
        scanned_snapshot(&cp, "/srv/exact.mp4", "hash-exact").await;
    let draft = linked_draft(file_version_id, media_snapshot_id);

    let created = cp.create_policy_input_set(draft).await.unwrap();
    let fetched = cp.get_policy_input_set(created.id).await.unwrap().unwrap();

    assert_eq!(
        fetched.media_snapshots[0].existing_media_snapshot_id,
        Some(media_snapshot_id)
    );
    assert_eq!(
        fetched.media_snapshots[0].target,
        PolicyInputTargetRef::FileVersion {
            id: file_version_id
        }
    );
}

#[tokio::test]
async fn generic_input_model_validation_precedes_live_and_closed_database() {
    let (live, live_tmp) = cp().await;
    let (file_version_id, media_snapshot_id) =
        scanned_snapshot(&live, "/srv/invalid.mp4", "hash-invalid").await;
    let observer = observer_for(&live_tmp).await;
    let before_events = event_count(&observer).await;
    let mut invalid = linked_draft(file_version_id, media_snapshot_id);
    invalid.slug = " ".to_owned();
    invalid.media_snapshots[0].target = TargetRef::MediaWork {
        id: MediaWorkId(9_999),
    };

    let live_err = live
        .create_policy_input_set(invalid.clone())
        .await
        .unwrap_err();
    assert_eq!(live_err.code(), "POLICY_VALIDATION_ERROR");
    assert_rejected_without_policy_state(
        &observer,
        before_events,
        &live_err,
        ErrorCode::PolicyValidationError,
    )
    .await;

    live.pool_for_test().close().await;
    let closed_err = live.create_policy_input_set(invalid).await.unwrap_err();
    assert_eq!(closed_err.code(), "POLICY_VALIDATION_ERROR");
    assert_rejected_without_policy_state(
        &observer,
        before_events,
        &closed_err,
        ErrorCode::PolicyValidationError,
    )
    .await;
}

#[tokio::test]
async fn generic_input_transaction_failure_precedes_link_validation() {
    let (cp, tmp) = cp().await;
    let (file_version_id, media_snapshot_id) =
        scanned_snapshot(&cp, "/srv/closed.mp4", "hash-closed").await;
    let observer = observer_for(&tmp).await;
    let before_events = event_count(&observer).await;
    let mut draft = linked_draft(file_version_id, media_snapshot_id);
    draft.media_snapshots[0].target = TargetRef::MediaWork {
        id: MediaWorkId(9_999),
    };
    cp.pool_for_test().close().await;

    let err = cp.create_policy_input_set(draft).await.unwrap_err();

    assert_rejected_without_policy_state(&observer, before_events, &err, ErrorCode::DbUnreachable)
        .await;
}

#[tokio::test]
async fn generic_input_missing_snapshot_is_not_found_without_policy_state() {
    let (cp, tmp) = cp().await;
    let (file_version_id, _) = scanned_snapshot(&cp, "/srv/missing.mp4", "hash-missing").await;
    let observer = observer_for(&tmp).await;
    let before_events = event_count(&observer).await;
    let draft = linked_draft(file_version_id, MediaSnapshotId(999_999));

    let err = cp.create_policy_input_set(draft).await.unwrap_err();

    assert_rejected_without_policy_state(&observer, before_events, &err, ErrorCode::NotFound).await;
}

#[tokio::test]
async fn generic_input_non_file_target_conflicts_without_policy_state() {
    let (cp, tmp) = cp().await;
    let (file_version_id, media_snapshot_id) =
        scanned_snapshot(&cp, "/srv/non-file.mp4", "hash-non-file").await;
    let observer = observer_for(&tmp).await;
    let before_events = event_count(&observer).await;
    let mut draft = linked_draft(file_version_id, media_snapshot_id);
    draft.media_snapshots[0].target = TargetRef::MediaWork {
        id: MediaWorkId(9_999),
    };

    let err = cp.create_policy_input_set(draft).await.unwrap_err();

    assert_rejected_without_policy_state(&observer, before_events, &err, ErrorCode::Conflict).await;
}

#[tokio::test]
async fn generic_input_checks_snapshot_existence_before_target_shape() {
    let (cp, tmp) = cp().await;
    let (file_version_id, _) =
        scanned_snapshot(&cp, "/srv/precedence.mp4", "hash-precedence").await;
    let observer = observer_for(&tmp).await;
    let before_events = event_count(&observer).await;
    let mut draft = linked_draft(file_version_id, MediaSnapshotId(999_999));
    draft.media_snapshots[0].target = TargetRef::MediaWork {
        id: MediaWorkId(9_999),
    };

    let err = cp.create_policy_input_set(draft).await.unwrap_err();

    assert_rejected_without_policy_state(&observer, before_events, &err, ErrorCode::NotFound).await;
}

#[tokio::test]
async fn generic_input_reports_link_errors_in_draft_order() {
    let (cp, tmp) = cp().await;
    let (first_version_id, first_snapshot_id) =
        scanned_snapshot(&cp, "/srv/first.mp4", "hash-first").await;
    let (second_version_id, second_snapshot_id) =
        scanned_snapshot(&cp, "/srv/second.mp4", "hash-second").await;
    let observer = observer_for(&tmp).await;
    let before_events = event_count(&observer).await;
    let mut draft = linked_draft(first_version_id, first_snapshot_id);
    let mut first_member = draft.media_snapshots[0].clone();
    first_member.ordinal = 0;
    first_member.existing_media_snapshot_id = Some(second_snapshot_id);
    first_member.target = TargetRef::FileVersion {
        id: first_version_id,
    };
    let mut second_member = draft.media_snapshots[0].clone();
    second_member.ordinal = 1;
    second_member.existing_media_snapshot_id = Some(first_snapshot_id);
    second_member.target = TargetRef::FileVersion {
        id: second_version_id,
    };
    draft.media_snapshots = vec![first_member, second_member];

    let err = cp.create_policy_input_set(draft).await.unwrap_err();

    assert_eq!(
        err.to_string(),
        format!(
            "conflict: media snapshot {second_snapshot_id} does not belong to \
             file version {first_version_id}"
        )
    );
    assert_rejected_without_policy_state(&observer, before_events, &err, ErrorCode::Conflict).await;
}

#[tokio::test]
async fn generic_input_later_mismatch_leaves_no_partial_policy_state() {
    let (cp, tmp) = cp().await;
    let (first_version_id, first_snapshot_id) =
        scanned_snapshot(&cp, "/srv/valid-first.mp4", "hash-valid-first").await;
    let (second_version_id, second_snapshot_id) =
        scanned_snapshot(&cp, "/srv/invalid-second.mp4", "hash-invalid-second").await;
    let observer = observer_for(&tmp).await;
    let before_events = event_count(&observer).await;
    let mut draft = linked_draft(first_version_id, first_snapshot_id);
    let mut second = draft.media_snapshots[0].clone();
    second.ordinal = 1;
    second.existing_media_snapshot_id = Some(second_snapshot_id);
    second.target = TargetRef::FileVersion {
        id: first_version_id,
    };
    draft.media_snapshots.push(second);

    let err = cp.create_policy_input_set(draft).await.unwrap_err();

    assert_eq!(
        err.to_string(),
        format!(
            "conflict: media snapshot {second_snapshot_id} does not belong to \
             file version {first_version_id}"
        )
    );
    assert_ne!(first_version_id, second_version_id);
    assert_rejected_without_policy_state(&observer, before_events, &err, ErrorCode::Conflict).await;
}

#[tokio::test]
async fn generic_input_identity_read_failure_precedes_member_conflict() {
    let (cp, tmp) = cp().await;
    let (file_version_id, media_snapshot_id) =
        scanned_snapshot(&cp, "/srv/read-failure.mp4", "hash-read-failure").await;
    let observer = observer_for(&tmp).await;
    let before_events = event_count(&observer).await;
    let mut draft = linked_draft(file_version_id, media_snapshot_id);
    draft.media_snapshots[0].target = TargetRef::MediaWork {
        id: MediaWorkId(9_999),
    };
    sqlx::query("ALTER TABLE media_snapshots RENAME TO unreadable_media_snapshots")
        .execute(cp.pool_for_test())
        .await
        .unwrap();

    let err = cp.create_policy_input_set(draft).await.unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::DbUnreachable);
    assert!(err.to_string().contains("media snapshot provenance lookup"));
    assert_rejected_without_policy_state(&observer, before_events, &err, ErrorCode::DbUnreachable)
        .await;
}

#[tokio::test]
async fn generic_input_accepts_exact_historical_file_version_link() {
    let (cp, _tmp) = cp().await;
    let (file_version_id, media_snapshot_id) =
        scanned_snapshot(&cp, "/srv/historical.mp4", "hash-historical").await;
    sqlx::query("UPDATE file_versions SET retired_at = '1970-01-01T00:00:01Z' WHERE id = ?")
        .bind(i64::try_from(file_version_id.0).unwrap())
        .execute(cp.pool_for_test())
        .await
        .unwrap();
    let draft = linked_draft(file_version_id, media_snapshot_id);

    let created = cp.create_policy_input_set(draft).await.unwrap();

    assert_eq!(created.media_snapshots.len(), 1);
    assert_eq!(
        created.media_snapshots[0].existing_media_snapshot_id,
        Some(media_snapshot_id)
    );
}

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
async fn over_budget_input_fails_before_database_access_without_durable_state() {
    let (cp, tmp) = cp().await;
    let observer = observer_for(&tmp).await;
    let before_events = event_count(&observer).await;
    let draft = draft_with_member_count(POLICY_INPUT_MAX_MEMBERS + 1);

    let err = cp.create_policy_input_set(draft).await.unwrap_err();

    assert_eq!(err.code(), "POLICY_VALIDATION_ERROR");
    assert_eq!(
        err.to_string(),
        format!(
            "policy validation error: policy input aggregate has {} members; maximum is {}",
            POLICY_INPUT_MAX_MEMBERS + 1,
            POLICY_INPUT_MAX_MEMBERS
        )
    );
    assert_rejected_without_policy_state(
        &observer,
        before_events,
        &err,
        ErrorCode::PolicyValidationError,
    )
    .await;
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

#[tokio::test]
async fn create_policy_input_set_from_scan_links_existing_rows() {
    let (cp, _tmp) = cp().await;
    let (file_version_id, media_snapshot_id) = scanned_snapshot(&cp, "/srv/a.mp4", "hash-a").await;

    let created = cp
        .create_policy_input_set_from_scan(PolicyInputFromScanInput {
            slug: "scan-h264".to_owned(),
            file_version_id,
            media_snapshot_id,
            container: "mp4".to_owned(),
            video_codec: "h264".to_owned(),
        })
        .await
        .unwrap();

    assert_eq!(created.slug, "scan-h264");
    assert_eq!(created.source_kind.as_str(), "imported");
    assert_eq!(created.file_version_id, file_version_id);
    assert_eq!(created.media_snapshot_id, media_snapshot_id);

    let input_set = cp
        .get_policy_input_set(created.input_set_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(input_set.slug, "scan-h264");
    assert_eq!(input_set.fixture_labels, ["scan-scan-h264"]);
    assert_eq!(input_set.media_snapshots.len(), 1);
    let media = &input_set.media_snapshots[0];
    assert_eq!(
        media.target,
        PolicyInputTargetRef::FileVersion {
            id: file_version_id
        }
    );
    assert_eq!(media.container.as_deref(), Some("mp4"));
    assert_eq!(media.video_codec.as_deref(), Some("h264"));
    assert_eq!(media.existing_media_snapshot_id, Some(media_snapshot_id));
}

#[tokio::test]
async fn input_from_scan_copies_snapshot_stream_facts() {
    let (cp, _tmp) = cp().await;
    let streams = json!([
        {
            "id": "stream-0",
            "index": 0,
            "kind": "video",
            "codec_name": "h264"
        },
        {
            "id": "stream-1",
            "index": 1,
            "kind": "video",
            "codec_name": "hevc"
        },
        {
            "id": "stream-2",
            "index": 2,
            "kind": "audio",
            "codec_name": "aac",
            "language": "eng"
        }
    ]);
    let payload = json!({
        "format": "test",
        "streams": streams.clone()
    });
    let (file_version_id, media_snapshot_id) =
        scanned_snapshot_with_payload(&cp, "/srv/a.mp4", "hash-a", payload).await;

    let created = cp
        .create_policy_input_set_from_scan(PolicyInputFromScanInput {
            slug: "scan-h264".to_owned(),
            file_version_id,
            media_snapshot_id,
            container: "mkv".to_owned(),
            video_codec: "hevc".to_owned(),
        })
        .await
        .unwrap();

    let input_set = cp
        .get_policy_input_set(created.input_set_id)
        .await
        .unwrap()
        .unwrap();
    let media = &input_set.media_snapshots[0];

    assert_eq!(media.container.as_deref(), Some("mkv"));
    assert_eq!(media.video_codec.as_deref(), Some("hevc"));
    assert_eq!(media.stream_summary["video_stream_count"], 2);
    assert_eq!(media.stream_summary["streams"], streams);
}

#[tokio::test]
async fn create_policy_input_set_from_scan_rejects_quarantined_only_location() {
    let (cp, _tmp) = cp().await;
    let (file_version_id, media_snapshot_id) =
        scanned_snapshot(&cp, "/legacy/movie.mp4", "hash-legacy").await;
    sqlx::query(
        "UPDATE file_locations SET address_state = 'unassigned_legacy', \
         storage_root_id = NULL, provider_relative_locator = NULL, \
         legacy_kind = 'local_path', legacy_locator = '/legacy/movie.mp4' \
         WHERE file_version_id = ?",
    )
    .bind(i64::try_from(file_version_id.0).unwrap())
    .execute(cp.pool_for_test())
    .await
    .unwrap();

    let err = cp
        .create_policy_input_set_from_scan(PolicyInputFromScanInput {
            slug: "quarantined".to_owned(),
            file_version_id,
            media_snapshot_id,
            container: "mp4".to_owned(),
            video_codec: "h264".to_owned(),
        })
        .await
        .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
    assert!(
        err.to_string()
            .contains("effectively available rooted location")
    );
    assert!(cp.list_policy_input_sets().await.unwrap().is_empty());
}

#[tokio::test]
async fn create_policy_input_set_from_scan_rejects_each_unavailable_root_state() {
    let (cp, _tmp) = cp().await;

    let disabled_root = library_root_at(&cp, "disabled-single", "/disabled-single").await;
    let disabled = scanned_snapshot_with_payload_on_root(
        &cp,
        disabled_root,
        "movie.mp4",
        "hash-disabled-single",
        json!({"format": "test", "streams": []}),
    )
    .await;
    cp.set_library_root_enabled(disabled_root, false)
        .await
        .unwrap();

    let unavailable_root = library_root_at(&cp, "unavailable-single", "/unavailable-single").await;
    let unavailable = scanned_snapshot_with_payload_on_root(
        &cp,
        unavailable_root,
        "movie.mp4",
        "hash-unavailable-single",
        json!({"format": "test", "streams": []}),
    )
    .await;
    cp.mark_library_root_unavailable(unavailable_root, "test validation loss".to_owned())
        .await
        .unwrap();

    let stale_root = library_root_at(&cp, "stale-single", "/stale-single").await;
    let stale = scanned_snapshot_with_payload_on_root(
        &cp,
        stale_root,
        "movie.mp4",
        "hash-stale-single",
        json!({"format": "test", "streams": []}),
    )
    .await;
    set_root_owner_status(&cp, stale_root, "stale").await;

    let retired_root = library_root_at(&cp, "retired-single", "/retired-single").await;
    let retired = scanned_snapshot_with_payload_on_root(
        &cp,
        retired_root,
        "movie.mp4",
        "hash-retired-single",
        json!({"format": "test", "streams": []}),
    )
    .await;
    set_root_owner_status(&cp, retired_root, "retired").await;

    for (slug, (file_version_id, media_snapshot_id)) in [
        ("disabled-single", disabled),
        ("unavailable-single", unavailable),
        ("stale-single", stale),
        ("retired-single", retired),
    ] {
        let err = cp
            .create_policy_input_set_from_scan(PolicyInputFromScanInput {
                slug: slug.to_owned(),
                file_version_id,
                media_snapshot_id,
                container: "mp4".to_owned(),
                video_codec: "h264".to_owned(),
            })
            .await
            .unwrap_err();
        assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
    }
    assert!(cp.list_policy_input_sets().await.unwrap().is_empty());
}

#[tokio::test]
async fn create_policy_input_set_from_scan_rejects_corrupt_library_enabled() {
    let (cp, _tmp) = cp().await;
    let root_id = library_root_at(&cp, "corrupt-enabled", "/corrupt-enabled").await;
    let (file_version_id, media_snapshot_id) = scanned_snapshot_with_payload_on_root(
        &cp,
        root_id,
        "movie.mp4",
        "hash-corrupt-enabled",
        json!({"format": "test", "streams": []}),
    )
    .await;
    let library_id: i64 = sqlx::query_scalar("SELECT library_id FROM library_roots WHERE id = ?")
        .bind(i64::try_from(root_id.0).unwrap())
        .fetch_one(cp.pool_for_test())
        .await
        .unwrap();
    with_check_constraints_disabled(cp.pool_for_test(), move |connection| {
        Box::pin(async move {
            sqlx::query("UPDATE libraries SET enabled = 2 WHERE id = ?")
                .bind(library_id)
                .execute(&mut *connection)
                .await?;
            Ok(())
        })
    })
    .await
    .unwrap();

    let error = cp
        .create_policy_input_set_from_scan(PolicyInputFromScanInput {
            slug: "corrupt-enabled".to_owned(),
            file_version_id,
            media_snapshot_id,
            container: "mp4".to_owned(),
            video_codec: "h264".to_owned(),
        })
        .await
        .unwrap_err();

    assert_eq!(error.error_code(), ErrorCode::DbUnreachable);
    assert!(error.to_string().contains("libraries.enabled"));
    assert!(cp.list_policy_input_sets().await.unwrap().is_empty());
}

#[tokio::test]
async fn create_policy_input_set_from_scan_accepts_another_available_location() {
    let (cp, _tmp) = cp().await;
    let unavailable_root = library_root_at(&cp, "unavailable-alias", "/unavailable-alias").await;
    let (file_version_id, media_snapshot_id) = scanned_snapshot_with_payload_on_root(
        &cp,
        unavailable_root,
        "movie.mp4",
        "hash-available-alias",
        json!({"format": "test", "streams": []}),
    )
    .await;
    cp.mark_library_root_unavailable(unavailable_root, "test validation loss".to_owned())
        .await
        .unwrap();
    let available_root = library_root_at(&cp, "available-alias", "/available-alias").await;
    cp.create_file_location(NewFileLocation {
        file_version_id,
        storage_root_id: available_root,
        provider_relative_locator: voom_store::test_support::test_relative_locator("movie.mp4"),
        proof: None,
        observed_at: T0,
    })
    .await
    .unwrap();

    let created = cp
        .create_policy_input_set_from_scan(PolicyInputFromScanInput {
            slug: "available-alias".to_owned(),
            file_version_id,
            media_snapshot_id,
            container: "mp4".to_owned(),
            video_codec: "h264".to_owned(),
        })
        .await
        .unwrap();

    assert_eq!(created.file_version_id, file_version_id);
}

#[tokio::test]
async fn create_policy_input_set_from_scan_rejects_missing_file_version() {
    let (cp, _tmp) = cp().await;
    let (_, media_snapshot_id) = scanned_snapshot(&cp, "/srv/a.mp4", "hash-a").await;

    let err = cp
        .create_policy_input_set_from_scan(PolicyInputFromScanInput {
            slug: "scan-h264".to_owned(),
            file_version_id: FileVersionId(999_999),
            media_snapshot_id,
            container: "mp4".to_owned(),
            video_codec: "h264".to_owned(),
        })
        .await
        .unwrap_err();

    assert_eq!(err.code(), ErrorCode::NotFound.as_str());
    assert!(cp.list_policy_input_sets().await.unwrap().is_empty());
}

#[tokio::test]
async fn create_policy_input_set_from_scan_rejects_missing_snapshot() {
    let (cp, _tmp) = cp().await;
    let (file_version_id, _) = scanned_snapshot(&cp, "/srv/a.mp4", "hash-a").await;

    let err = cp
        .create_policy_input_set_from_scan(PolicyInputFromScanInput {
            slug: "scan-h264".to_owned(),
            file_version_id,
            media_snapshot_id: MediaSnapshotId(999_999),
            container: "mp4".to_owned(),
            video_codec: "h264".to_owned(),
        })
        .await
        .unwrap_err();

    assert_eq!(err.code(), ErrorCode::NotFound.as_str());
    assert!(cp.list_policy_input_sets().await.unwrap().is_empty());
}

#[tokio::test]
async fn create_policy_input_set_from_scan_rejects_snapshot_for_other_file_version() {
    let (cp, _tmp) = cp().await;
    let (_, media_snapshot_id) = scanned_snapshot(&cp, "/srv/a.mp4", "hash-a").await;
    let (other_file_version_id, _) = scanned_snapshot(&cp, "/srv/b.mp4", "hash-b").await;

    let err = cp
        .create_policy_input_set_from_scan(PolicyInputFromScanInput {
            slug: "scan-h264".to_owned(),
            file_version_id: other_file_version_id,
            media_snapshot_id,
            container: "mp4".to_owned(),
            video_codec: "h264".to_owned(),
        })
        .await
        .unwrap_err();

    assert_eq!(err.code(), ErrorCode::Conflict.as_str());
    assert!(cp.list_policy_input_sets().await.unwrap().is_empty());
}

#[tokio::test]
async fn whole_scan_includes_video_and_skips_non_video() {
    let (cp, _tmp) = cp().await;
    let video_payload = json!({
        "container": "mp4",
        "video_codec": "h264",
        "streams": [
            {"id": "v-0", "index": 0, "kind": "video", "codec_name": "h264"},
            {"id": "a-0", "index": 1, "kind": "audio", "codec_name": "aac", "language": "eng"}
        ]
    });
    let audio_payload = json!({
        "container": "mp4",
        "streams": [
            {"id": "a-0", "index": 0, "kind": "audio", "codec_name": "aac", "language": "eng"}
        ]
    });
    let (video_file_version_id, video_media_snapshot_id) =
        scanned_snapshot_with_payload(&cp, "/srv/movie.mp4", "hash-video", video_payload).await;
    let (_audio_file_version_id, _audio_media_snapshot_id) =
        scanned_snapshot_with_payload(&cp, "/srv/song.m4a", "hash-audio", audio_payload).await;

    let result = cp
        .create_policy_input_set_from_whole_scan(WholeScanInput {
            slug: "whole-library".to_owned(),
        })
        .await
        .unwrap();

    assert_eq!(result.slug, "whole-library");
    assert_eq!(result.included_count, 1);
    assert_eq!(result.skipped_count, 1);

    let input_set = cp
        .get_policy_input_set(result.input_set_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(input_set.media_snapshots.len(), 1);
    let media = &input_set.media_snapshots[0];
    assert_eq!(
        media.target,
        PolicyInputTargetRef::FileVersion {
            id: video_file_version_id
        }
    );
    assert_eq!(media.container.as_deref(), Some("mp4"));
    assert_eq!(media.video_codec.as_deref(), Some("h264"));
    assert_eq!(
        media.existing_media_snapshot_id,
        Some(video_media_snapshot_id)
    );
}

#[tokio::test]
async fn whole_scan_skips_quarantined_and_unavailable_locations() {
    let (cp, _tmp) = cp().await;
    let video = || {
        json!({
            "container": "mp4",
            "video_codec": "h264",
            "streams": [{"id": "v-0", "index": 0, "kind": "video", "codec_name": "h264"}]
        })
    };
    let disabled_root = library_root_at(&cp, "disabled", "/media/disabled").await;
    scanned_snapshot_with_payload_on_root(
        &cp,
        disabled_root,
        "disabled.mp4",
        "hash-disabled",
        video(),
    )
    .await;
    cp.set_library_root_enabled(disabled_root, false)
        .await
        .unwrap();

    let (legacy_version, _) =
        scanned_snapshot_with_payload(&cp, "/legacy/movie.mp4", "hash-legacy", video()).await;
    sqlx::query(
        "UPDATE file_locations SET address_state = 'unassigned_legacy', \
         storage_root_id = NULL, provider_relative_locator = NULL, \
         legacy_kind = 'local_path', legacy_locator = '/legacy/movie.mp4' \
         WHERE file_version_id = ?",
    )
    .bind(i64::try_from(legacy_version.0).unwrap())
    .execute(cp.pool_for_test())
    .await
    .unwrap();

    let (eligible_version, _) =
        scanned_snapshot_with_payload(&cp, "/media/eligible.mp4", "hash-eligible", video()).await;

    let result = cp
        .create_policy_input_set_from_whole_scan(WholeScanInput {
            slug: "available-only".to_owned(),
        })
        .await
        .unwrap();

    assert_eq!(result.included_count, 1);
    assert_eq!(result.skipped_count, 2);
    let input_set = cp
        .get_policy_input_set(result.input_set_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(input_set.media_snapshots.len(), 1);
    assert_eq!(
        input_set.media_snapshots[0].target,
        PolicyInputTargetRef::FileVersion {
            id: eligible_version
        }
    );
}

#[tokio::test]
async fn whole_scan_empty_database_creates_durable_zero_member_input() {
    let (cp, _tmp) = cp().await;
    let before_events = event_count(&cp).await;

    let result = cp
        .create_policy_input_set_from_whole_scan(WholeScanInput {
            slug: "empty-library".to_owned(),
        })
        .await
        .unwrap();

    assert_eq!(result.included_count, 0);
    assert_eq!(result.skipped_count, 0);
    let input = cp
        .get_policy_input_set(result.input_set_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(input.source_kind, PolicyInputSourceKind::Imported);
    assert_eq!(input.fixture_labels, ["whole-scan-empty-library"]);
    assert!(input.media_snapshots.is_empty());
    assert_eq!(
        policy_input_aggregate_counts(&cp).await,
        PolicyInputAggregateCounts {
            input_sets: 1,
            fixture_labels: 1,
            ..PolicyInputAggregateCounts::default()
        }
    );
    assert_eq!(event_count(&cp).await, before_events);
}

#[tokio::test]
async fn whole_scan_all_non_video_creates_empty_input_and_counts_skip() {
    let (cp, _tmp) = cp().await;
    scanned_snapshot_with_payload(
        &cp,
        "/srv/song.m4a",
        "hash-audio-only",
        json!({
            "container": "mp4",
            "streams": [
                {"id": "a-0", "index": 0, "kind": "audio", "codec_name": "aac"}
            ]
        }),
    )
    .await;

    let result = cp
        .create_policy_input_set_from_whole_scan(WholeScanInput {
            slug: "audio-only-library".to_owned(),
        })
        .await
        .unwrap();

    assert_eq!(result.included_count, 0);
    assert_eq!(result.skipped_count, 1);
    let input = cp
        .get_policy_input_set(result.input_set_id)
        .await
        .unwrap()
        .unwrap();
    assert!(input.media_snapshots.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_empty_whole_scan_creation_commits_one_complete_aggregate() {
    let (cp, _tmp) = cp().await;
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
    let mut handles = Vec::new();
    for _ in 0..2 {
        let cp = cp.clone();
        let barrier = barrier.clone();
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            cp.create_policy_input_set_from_whole_scan(WholeScanInput {
                slug: "concurrent-empty".to_owned(),
            })
            .await
        }));
    }

    let mut successes = 0;
    let mut failures = 0;
    for handle in handles {
        match handle.await.unwrap() {
            Ok(_) => successes += 1,
            Err(error) => {
                assert_eq!(error.error_code(), ErrorCode::DbUnreachable);
                failures += 1;
            }
        }
    }

    assert_eq!((successes, failures), (1, 1));
    assert_eq!(
        policy_input_aggregate_counts(&cp).await,
        PolicyInputAggregateCounts {
            input_sets: 1,
            fixture_labels: 1,
            ..PolicyInputAggregateCounts::default()
        }
    );
}

#[tokio::test]
async fn empty_scan_fixture_insert_failure_rolls_back_parent_and_emits_no_event() {
    let (cp, _tmp) = cp().await;
    let before_events = event_count(&cp).await;
    sqlx::query(
        "CREATE TRIGGER fail_empty_scan_label \
         BEFORE INSERT ON policy_input_set_fixture_labels \
         BEGIN SELECT RAISE(ABORT, 'forced empty scan label failure'); END",
    )
    .execute(cp.pool_for_test())
    .await
    .unwrap();

    let error = cp
        .create_policy_input_set_from_whole_scan(WholeScanInput {
            slug: "rollback-empty".to_owned(),
        })
        .await
        .unwrap_err();

    assert_eq!(error.error_code(), ErrorCode::DbUnreachable);
    assert_eq!(
        policy_input_aggregate_counts(&cp).await,
        PolicyInputAggregateCounts::default()
    );
    assert_eq!(event_count(&cp).await, before_events);
}

#[tokio::test]
async fn generic_targetless_import_remains_invalid_without_durable_state() {
    let (cp, _tmp) = cp().await;
    let before_events = event_count(&cp).await;
    let mut draft = load_fixture(FixtureName::SyntheticCompliantBaseline).unwrap();
    draft.source_kind = PolicyInputSourceKind::Imported;
    draft.synthetic_targets.clear();
    draft.media_snapshots.clear();

    let error = cp.create_policy_input_set(draft).await.unwrap_err();

    assert_eq!(error.error_code(), ErrorCode::PolicyValidationError);
    assert_eq!(
        policy_input_aggregate_counts(&cp).await,
        PolicyInputAggregateCounts::default()
    );
    assert_eq!(event_count(&cp).await, before_events);
}

async fn library_root_at(
    cp: &crate::ControlPlane,
    slug: &str,
    path: &str,
) -> voom_core::StorageRootId {
    let lib = cp
        .create_library(NewLibrary {
            slug: slug.to_owned(),
            display_name: slug.to_owned(),
            media_kind: LibraryMediaKind::Movie,
            description: None,
            enabled: true,
        })
        .await
        .unwrap();
    let owner_id = sqlx::query(
        "INSERT INTO nodes \
         (name, kind, status, registered_at, last_seen_at, heartbeat_ttl_seconds, \
          auth_token_hash, auth_token_hint, metadata) \
         VALUES (?, 'local', 'active', '1970-01-01T00:00:00Z', \
                 '1970-01-01T00:00:00Z', 60, 'hash', 'hint', '{}')",
    )
    .bind(format!("{slug}-owner"))
    .execute(cp.pool_for_test())
    .await
    .unwrap()
    .last_insert_rowid();
    let root = cp
        .create_library_root(NewLibraryRoot {
            library_id: lib.id,
            owner_node_id: voom_core::NodeId(u64::try_from(owner_id).unwrap()),
            provider_kind: voom_core::StorageProviderKind::LocalFilesystem,
            provider_locator: voom_core::ProviderLocator::new(path.to_owned()).unwrap(),
            display_locator: path.to_owned(),
            include_globs: Vec::new(),
            exclude_globs: Vec::new(),
            extension_allowlist: Vec::new(),
            scan_mode: LibraryScanMode::ManualRecursive,
            symlink_policy: SymlinkPolicy::Reject,
            hidden_file_policy: HiddenFilePolicy::Ignore,
            max_depth: None,
            stability_seconds: 0,
            debounce_seconds: 0,
            default_output_root_id: None,
            default_staging_root_id: None,
            default_backup_root_id: None,
            enabled: true,
        })
        .await
        .unwrap();
    cp.activate_library_root(root.id, format!("test:{slug}"))
        .await
        .unwrap();
    root.id
}

#[tokio::test]
async fn root_scoped_scan_includes_only_files_under_the_root() {
    let (cp, _tmp) = cp().await;
    let root_id = library_root_at(&cp, "films", "/media/films").await;
    let video = || {
        json!({
            "container": "mp4",
            "video_codec": "h264",
            "streams": [{"id": "v-0", "index": 0, "kind": "video", "codec_name": "h264"}]
        })
    };
    // Under the root.
    let (under, _) =
        scanned_snapshot_with_payload_on_root(&cp, root_id, "a.mp4", "hash-a", video()).await;
    // Different storage root.
    scanned_snapshot_with_payload(&cp, "/media/shows/b.mp4", "hash-b", video()).await;
    // A textual prefix is irrelevant when the durable root identity differs.
    scanned_snapshot_with_payload(&cp, "/media/films-4k/c.mp4", "hash-c", video()).await;

    let result = cp
        .create_policy_input_set_from_root(RootScopedScanInput {
            slug: "films-input".to_owned(),
            library_root_id: root_id,
        })
        .await
        .unwrap();

    assert_eq!(result.included_count, 1);
    assert_eq!(result.skipped_count, 2);
    let input_set = cp
        .get_policy_input_set(result.input_set_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(input_set.media_snapshots.len(), 1);
    assert_eq!(
        input_set.media_snapshots[0].target,
        PolicyInputTargetRef::FileVersion { id: under }
    );
}

#[tokio::test]
async fn root_scoped_scan_with_no_eligible_files_creates_durable_empty_input() {
    let (cp, _tmp) = cp().await;
    let root_id = library_root_at(&cp, "empty-root-library", "/media/empty").await;

    let result = cp
        .create_policy_input_set_from_root(RootScopedScanInput {
            slug: "empty-root".to_owned(),
            library_root_id: root_id,
        })
        .await
        .unwrap();

    assert_eq!(result.included_count, 0);
    assert_eq!(result.skipped_count, 0);
    let input = cp
        .get_policy_input_set(result.input_set_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(input.fixture_labels, ["root-scan-empty-root"]);
    assert!(input.media_snapshots.is_empty());
    assert_eq!(
        policy_input_aggregate_counts(&cp).await,
        PolicyInputAggregateCounts {
            input_sets: 1,
            fixture_labels: 1,
            ..PolicyInputAggregateCounts::default()
        }
    );
}

#[tokio::test]
async fn root_scoped_scan_missing_root_is_not_found() {
    let (cp, _tmp) = cp().await;
    let err = cp
        .create_policy_input_set_from_root(RootScopedScanInput {
            slug: "x".to_owned(),
            library_root_id: voom_core::StorageRootId(999),
        })
        .await
        .unwrap_err();
    assert_eq!(err.error_code(), ErrorCode::NotFound);
}

async fn scanned_snapshot(
    cp: &crate::ControlPlane,
    path: &str,
    hash: &str,
) -> (FileVersionId, MediaSnapshotId) {
    scanned_snapshot_with_payload(cp, path, hash, json!({"format": "test", "streams": []})).await
}

async fn set_root_owner_status(
    cp: &crate::ControlPlane,
    root_id: voom_core::StorageRootId,
    status: &str,
) {
    let owner_node_id: i64 =
        sqlx::query_scalar("SELECT owner_node_id FROM library_roots WHERE id = ?")
            .bind(i64::try_from(root_id.0).unwrap())
            .fetch_one(cp.pool_for_test())
            .await
            .unwrap();
    sqlx::query(
        "UPDATE nodes SET status = ?, \
         retired_at = CASE WHEN ? = 'retired' THEN '1970-01-01T00:00:00Z' END \
         WHERE id = ?",
    )
    .bind(status)
    .bind(status)
    .bind(owner_node_id)
    .execute(cp.pool_for_test())
    .await
    .unwrap();
}

async fn scanned_snapshot_with_payload(
    cp: &crate::ControlPlane,
    path: &str,
    hash: &str,
    payload: serde_json::Value,
) -> (FileVersionId, MediaSnapshotId) {
    scanned_snapshot_with_payload_on_root(
        cp,
        voom_store::test_support::TEST_STORAGE_ROOT_ID,
        path,
        hash,
        payload,
    )
    .await
}

async fn scanned_snapshot_with_payload_on_root(
    cp: &crate::ControlPlane,
    storage_root_id: voom_core::StorageRootId,
    path: &str,
    hash: &str,
    payload: serde_json::Value,
) -> (FileVersionId, MediaSnapshotId) {
    let outcome = cp
        .record_discovered_file(
            DiscoveredFile {
                storage_root_id,
                provider_relative_locator: voom_store::test_support::test_relative_locator(path),
                content_hash: hash.to_owned(),
                size_bytes: 1024,
                observed_at: T0,
                proof: None,
            },
            None,
        )
        .await
        .unwrap();
    let IngestOutcome::NewFileAsset {
        file_version_id, ..
    } = outcome
    else {
        panic!("expected new file asset");
    };
    let snapshot = cp
        .record_media_snapshot(file_version_id, None, payload, T0)
        .await
        .unwrap();
    (file_version_id, snapshot.id)
}
