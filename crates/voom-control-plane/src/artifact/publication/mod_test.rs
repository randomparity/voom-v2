use super::*;

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio::sync::Notify;

#[test]
fn stored_compatibility_corruption_is_database_error() {
    let payload = json!({"media_dispatch": {"operation": "transcode_audio", "settings": {
        "compatibility": {"source_channels": -1}
    }}});
    let error = stored_compatibility_settings(&payload).unwrap_err();
    assert!(matches!(error, VoomError::Database { .. }));
    assert!(
        stored_compatibility_settings(&json!({"media_dispatch": {"operation": "remux"}}))
            .unwrap()
            .is_none()
    );
}

#[test]
fn required_reprobe_rejects_unusable_media_facts() {
    for snapshot in [
        json!({"streams": [{"index": 0, "kind": "video", "codec_name": "hevc"}]}),
        json!({"container": {"format_name": "matroska"}, "streams": [{"index": 0, "codec_name": "hevc"}]}),
        json!({"container": {"format_name": "matroska"}, "streams": [{"index": 0, "kind": "video"}]}),
        json!({"container": {"format_name": "matroska"}, "streams": [{"index": 0, "kind": "audio"}]}),
    ] {
        assert!(
            normalized_snapshot(snapshot).is_err(),
            "a required reprobe must be usable by the next phase"
        );
    }
}

#[test]
fn required_reprobe_preserves_metadata_and_assigns_stable_stream_ids() {
    let normalized = normalized_snapshot(json!({
        "container": {"format_name": "matroska"},
        "streams": [{"index": 2, "kind": "audio", "codec_name": "aac", "channels": 2}],
        "provider_extension": {"value": true},
    }))
    .unwrap();
    assert_eq!(normalized["streams"][0]["id"], "stream-2");
    assert_eq!(normalized["streams"][0]["channels"], 2);
    assert_eq!(normalized["provider_extension"]["value"], true);
    assert!(normalized_snapshot(json!({
        "container": {"format_name": "matroska"},
        "streams": [{"index": 1, "kind": "audio", "codec_name": "aac"}, {"index": 1, "kind": "audio", "codec_name": "aac"}],
    })).is_err());
}

#[test]
fn mutation_result_rejects_a_changed_worker_source() {
    let raw = json!({
        "status": "remuxed", "provider": "test", "provider_version": "1",
        "input_pre": {"size_bytes": 1, "content_hash": "before"},
        "input_post": {"size_bytes": 1, "content_hash": "after"},
        "output": {"size_bytes": 2, "content_hash": "output"},
        "output_container": "mkv", "kept_snapshot_stream_ids": ["stream-0"], "default_snapshot_stream_ids": [],
    });
    assert!(
        validate_worker_result(OperationKind::Remux, &raw).is_err(),
        "the worker and owner source facts must describe an unchanged input"
    );
}

#[test]
fn synthesis_probe_comparison_uses_exact_admitted_companion_projection() {
    use voom_store::repo::media::audio_synthesis_operations::AudioSynthesisCompanion;
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/owner-publication-synthesis-probe.json"
    ))
    .unwrap();
    let output: voom_worker_protocol::OwnerObservedOutput =
        serde_json::from_value(fixture["original_output"].clone()).unwrap();
    let probe: OwnerProbeEvidence =
        serde_json::from_value(fixture["canonical_probe"].clone()).unwrap();
    let result_id = fixture["worker_result"]["selected_output_streams"][0]["snapshot_stream_id"]
        .as_str()
        .unwrap();
    let companions = [AudioSynthesisCompanion {
        id: 1,
        operation_id: 1,
        ordinal: 0,
        companion_id: "companion".to_owned(),
        source_snapshot_stream_id: "stream-1".to_owned(),
        source_provider_stream_index: 1,
        result_snapshot_stream_id: result_id.to_owned(),
        result_provider_stream_index: Some(2),
        codec: Some("aac".to_owned()),
        channels: Some(2),
        language: Some("eng".to_owned()),
        title: None,
        disposition_default: Some(true),
        disposition_forced: Some(false),
        disposition_commentary: Some(false),
        lineage_id: None,
    }];
    validate_original_probe(
        &output,
        &probe,
        Some(&companions),
        &fixture["worker_result"],
        None,
    )
    .unwrap();
    assert!(
        validate_original_probe(&output, &probe, None, &fixture["worker_result"], None).is_err(),
        "a stable companion ID requires the exact admitted operation anchor"
    );
    let mut wrong_worker = probe.clone();
    wrong_worker.worker_id = WorkerId(probe.worker_id.0 + 1);
    assert!(
        validate_original_probe(
            &output,
            &wrong_worker,
            Some(&companions),
            &fixture["worker_result"],
            None,
        )
        .is_err()
    );
    let mut wrong_stream = probe.clone();
    wrong_stream.result.snapshot["streams"][2]["id"] = json!("foreign-companion");
    assert!(
        validate_original_probe(
            &output,
            &wrong_stream,
            Some(&companions),
            &fixture["worker_result"],
            None,
        )
        .is_err()
    );
    let mut wrong_facts = probe.clone();
    wrong_facts.result.snapshot["streams"][2]["channels"] = json!(6);
    assert!(
        validate_original_probe(
            &output,
            &wrong_facts,
            Some(&companions),
            &fixture["worker_result"],
            None,
        )
        .is_err()
    );
    let mut wrong_companions = companions.clone();
    wrong_companions[0].result_snapshot_stream_id = "foreign-operation-companion".to_owned();
    assert!(
        validate_original_probe(
            &output,
            &probe,
            Some(&wrong_companions),
            &fixture["worker_result"],
            None,
        )
        .is_err()
    );
}

#[test]
fn owner_result_projection_keeps_unknown_worker_fields_and_original_facts() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/owner-publication-synthesis-probe.json"
    ))
    .unwrap();
    let mut complete = fixture["worker_result"].clone();
    complete["agent_observed"] = json!({"owner": "extension"});
    complete["artifact_access"] = json!({"access": "extension"});
    let original = complete.clone();
    let projected = owner_worker_result(&complete).unwrap();
    serde_json::from_value::<voom_worker_protocol::TranscodeAudioResult>(projected.clone())
        .unwrap();
    assert_eq!(projected, fixture["worker_result"]);
    assert_eq!(
        complete, original,
        "projection must not rewrite retained completion evidence"
    );
    complete["unexpected_worker_field"] = json!(true);
    assert!(
        serde_json::from_value::<voom_worker_protocol::TranscodeAudioResult>(
            owner_worker_result(&complete).unwrap()
        )
        .is_err(),
        "owner projection must not erase an unaccepted worker field"
    );
}

struct WaitGate {
    acquisitions: AtomicUsize,
    at_boundary: Notify,
    published: Notify,
}

struct WaitFixture {
    cp: ControlPlane,
    publisher: sqlx::SqlitePool,
    pending: PendingMedia,
    gate: Arc<WaitGate>,
    _database: voom_test_support::TempDatabase,
}

async fn waiter_fixture() -> WaitFixture {
    let database = voom_test_support::TempDatabase::new().unwrap();
    let url = format!("sqlite://{}", database.path().display());
    voom_store::init(&url).await.unwrap();
    let gate = Arc::new(WaitGate {
        acquisitions: AtomicUsize::new(usize::MAX),
        at_boundary: Notify::new(),
        published: Notify::new(),
    });
    let callback_gate = gate.clone();
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .min_connections(1)
        .max_lifetime(None)
        .idle_timeout(None)
        .before_acquire(move |_, _| {
            let gate = callback_gate.clone();
            Box::pin(async move {
                if gate.acquisitions.load(Ordering::SeqCst) != usize::MAX
                    && gate.acquisitions.fetch_add(1, Ordering::SeqCst) == 1
                {
                    gate.at_boundary.notify_one();
                    tokio::time::timeout(
                        std::time::Duration::from_secs(5),
                        gate.published.notified(),
                    )
                    .await
                    .map_err(|_| sqlx::Error::PoolTimedOut)?;
                }
                Ok(true)
            })
        })
        .connect(&url)
        .await
        .unwrap();
    let cp = ControlPlane::open_with_pool(
        pool.clone(),
        Arc::new(voom_core::clock_test_support::FrozenClock::new(
            OffsetDateTime::UNIX_EPOCH,
        )),
    )
    .await
    .unwrap();
    voom_store::test_support::seed_test_rooted_location(&pool)
        .await
        .unwrap();
    let owner = voom_test_support::commit_node::SimulatedOwnerNode::new().unwrap();
    owner.install(&pool).await.unwrap();
    sqlx::query("INSERT INTO workers (id,name,kind,status,registered_at,last_seen_at,node_id,node_incarnation_id) VALUES (1,'waiter','remote','active','1970-01-01T00:00:00Z','1970-01-01T00:00:00Z',9000001,?)")
        .bind(owner.incarnation_id.to_string()).execute(&pool).await.unwrap();
    sqlx::raw_sql(r#"
        INSERT INTO jobs (id,kind,state,created_at,updated_at)
        VALUES (1,'workflow','open','1970-01-01T00:00:00Z','1970-01-01T00:00:00Z');
        INSERT INTO tickets (id,job_id,kind,state,payload,next_eligible_at,created_at,state_changed_at)
        VALUES (1,1,'remux','leased',
        '{"rendered_payload":{"source_file_version_id":9000001,"source_location_id":9000001}}',
        '1970-01-01T00:00:00Z','1970-01-01T00:00:00Z','1970-01-01T00:00:00Z');
        INSERT INTO leases (id,ticket_id,worker_id,state,acquired_at,expires_at,last_heartbeat_at,ttl_seconds)
        VALUES (1,1,1,'held','1970-01-01T00:00:00Z','1970-01-01T00:01:00Z',
        '1970-01-01T00:00:00Z',60);
        INSERT INTO artifact_access_plans (id,lease_id,ticket_id,worker_id,node_id,owner_node_id,
        access_evidence,status,created_at,updated_at)
        VALUES (1,1,1,1,9000001,9000001,
        '{"declaration":[{"target":{"kind":"storage_root","storage_root_id":9000001},"rights":["read"]}],"root_epochs":[{"storage_root_id":9000001,"root_epoch":1}]}',
        'selected','1970-01-01T00:00:00Z','1970-01-01T00:00:00Z');
    "#).execute(&pool).await.unwrap();
    let context = RemoteMediaCompletionContext {
        node_id: owner.node_id,
        incarnation_id: owner.incarnation_id,
        worker_id: WorkerId(1),
        lease_id: LeaseId(1),
        ticket_id: TicketId(1),
        job_id: JobId(1),
        source_file_version_id: FileVersionId(9_000_001),
        source_location_id: FileLocationId(9_000_001),
        source_location_fence: Some(SourceLocationFence {
            epoch: 0,
            storage_root_id: voom_core::StorageRootId(9_000_001),
            provider_relative_locator: ProviderRelativeLocator::new(
                "workflow-fixture.mkv".to_owned(),
            )
            .unwrap(),
        }),
        idempotency_key: "waiter-completion".to_owned(),
        request_hash: "waiter-hash".to_owned(),
        result: json!({}),
    };
    WaitFixture {
        cp,
        publisher: voom_store::connect(&url).await.unwrap(),
        pending: PendingMedia {
            context,
            commits: Vec::new(),
        },
        gate,
        _database: database,
    }
}

#[tokio::test]
async fn waiter_replays_publication_committed_between_snapshots() {
    let WaitFixture {
        cp,
        publisher,
        pending,
        gate,
        _database,
    } = waiter_fixture().await;
    let context = pending.context.clone();
    let plan = cp
        .artifact_access_plans
        .get_by_lease(context.lease_id)
        .await
        .unwrap()
        .unwrap();
    let expected = RemoteCompleteOutcome {
        lease_id: context.lease_id,
        ticket_id: context.ticket_id,
        worker_id: context.worker_id,
        artifact_access_plan: crate::cases::execution::remote_execution::remote_plan(&plan),
    };
    let publish = async {
        gate.at_boundary.notified().await;
        let mut tx = voom_store::tx::begin_write_first(&publisher, "waiter test publish")
            .await
            .unwrap();
        sqlx::query("UPDATE leases SET state='released',release_reason='released',released_at='1970-01-01T00:00:00Z' WHERE id=1")
            .execute(&mut *tx).await.unwrap();
        sqlx::query("UPDATE tickets SET state='succeeded' WHERE id=1")
            .execute(&mut *tx)
            .await
            .unwrap();
        let replay = context.replay_input(OffsetDateTime::UNIX_EPOCH);
        assert!(matches!(
            cp.remote_idempotency
                .reserve_or_replay_in_tx(&mut tx, replay.clone())
                .await
                .unwrap(),
            IdempotencyOutcome::Reserved
        ));
        cp.complete_remote_ok_in_tx(
            &mut tx,
            context.node_id,
            &replay.route_key,
            Some(context.worker_id),
            &replay.idempotency_key,
            &expected,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        gate.published.notify_one();
    };
    gate.acquisitions.store(0, Ordering::SeqCst);
    let (observed, ()) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(pending.wait(&cp), publish)
    })
    .await
    .unwrap();
    assert_eq!(
        serde_json::to_value(observed.unwrap()).unwrap(),
        serde_json::to_value(&expected).unwrap(),
        "publication must return its persisted replay, not a lost-lease conflict"
    );
    assert_eq!(
        gate.acquisitions.load(Ordering::SeqCst),
        2,
        "the isolated pool must reach exactly the forced snapshot boundary"
    );
}

#[tokio::test]
async fn waiter_without_replay_rejects_expired_or_released_source() {
    for (state, expiry) in [
        ("held", "1970-01-01T00:00:00Z"),
        ("released", "1970-01-01T00:01:00Z"),
    ] {
        let fixture = waiter_fixture().await;
        sqlx::query("UPDATE leases SET state=?,expires_at=?,release_reason=CASE WHEN ?='held' THEN NULL ELSE 'released' END,released_at=CASE WHEN ?='held' THEN NULL ELSE '1970-01-01T00:00:00Z' END WHERE id=1")
            .bind(state).bind(expiry).bind(state).bind(state).execute(&fixture.publisher).await.unwrap();
        let error = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            fixture.pending.wait(&fixture.cp),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert!(
            matches!(error, VoomError::Conflict(_)),
            "without terminal replay, expired/non-held authority must fail"
        );
    }
}
