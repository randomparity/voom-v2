use super::*;

#[tokio::test]
async fn refresh_counts_uses_job_wall_time_and_overlapping_durable_leases() {
    let (control, _db) = crate::cases::cp().await;
    sqlx::query(
        "INSERT INTO jobs (id, kind, state, priority, created_at, updated_at) \
         VALUES (71, 'workflow', 'open', 0, '2026-01-01T00:00:00Z', \
                 '2026-01-01T00:00:00Z')",
    )
    .execute(&control.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO workers \
         (id, name, kind, status, registered_at, last_seen_at) VALUES \
         (81, 'summary-worker', 'synthetic', 'active', \
          '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
    )
    .execute(&control.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO tickets \
         (id, job_id, kind, state, payload, attempt, max_attempts, next_eligible_at, \
          created_at, state_changed_at) VALUES \
         (91, 71, 'synthetic.workflow.operation.test', 'succeeded', '{}', 1, 1, \
          '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', '2026-01-01T00:00:10Z'), \
         (92, 71, 'synthetic.workflow.operation.test', 'succeeded', '{}', 1, 1, \
          '2026-01-01T00:00:05Z', '2026-01-01T00:00:05Z', '2026-01-01T00:00:15Z')",
    )
    .execute(&control.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO leases \
         (id, ticket_id, worker_id, state, acquired_at, expires_at, last_heartbeat_at, \
          ttl_seconds, release_reason, released_at) VALUES \
         (101, 91, 81, 'released', '2026-01-01T00:00:00Z', \
          '2026-01-01T00:01:00Z', '2026-01-01T00:00:00Z', 60, 'succeeded', \
          '2026-01-01T00:00:10Z'), \
         (102, 92, 81, 'released', '2026-01-01T00:00:05Z', \
          '2026-01-01T00:01:05Z', '2026-01-01T00:00:05Z', 60, 'succeeded', \
          '2026-01-01T00:00:15Z')",
    )
    .execute(&control.pool)
    .await
    .unwrap();
    let elapsed = Duration::from_secs(20);
    let mut summary = WorkflowRunSummary::empty(JobId(71), Duration::from_secs(3));

    summary
        .refresh_counts(&control.tickets, &control.leases, JobId(71), elapsed)
        .await
        .unwrap();

    assert_eq!(summary.elapsed, elapsed);
    assert_eq!(summary.ticket_count, 2);
    assert_eq!(summary.peak_active_workflow_leases, 2);
}

#[tokio::test]
async fn refresh_counts_fails_loudly_when_the_pool_is_closed() {
    let (control, _db) = crate::cases::cp().await;
    let mut summary = WorkflowRunSummary::empty(JobId(71), Duration::from_secs(3));
    control.pool.close().await;

    let result = summary
        .refresh_counts(
            &control.tickets,
            &control.leases,
            JobId(71),
            Duration::from_secs(20),
        )
        .await;

    assert!(result.is_err());
}

/// Pins what an undecodable payload costs a summary, so a later change to it is
/// deliberate. After the ADR 0069 payload break this is the shape of a completed
/// pre-upgrade byte-touching row, which the upgrade drain does not cover — the
/// drain names *unfinished* tickets — so such a row is skipped here forever.
///
/// The two counts disagree by design: `ticket_count` is taken from the row count
/// before any payload is read, while `branch_count` and the per-operation totals
/// are built by decoding. Recording the inconsistency beats papering over it with
/// an in-memory counter, which would leave the durable summary row exactly as
/// inconsistent while adding a `merge_invocation` rule a later resume can get wrong.
#[tokio::test]
async fn an_undecodable_ticket_is_counted_in_the_total_and_missing_from_per_operation() {
    let (control, _db) = crate::cases::cp().await;
    sqlx::query(
        "INSERT INTO jobs (id, kind, state, priority, created_at, updated_at) \
         VALUES (73, 'workflow', 'open', 0, '2026-01-01T00:00:00Z', \
                 '2026-01-01T00:00:00Z')",
    )
    .execute(&control.pool)
    .await
    .unwrap();
    let decodable = crate::workflow::plan::ticket_payload::WorkflowTicketPayload::new_for_test(
        "wf-73",
        "plan-73",
        "node-a",
        "branch-a",
        OperationKind::IdentifyMedia,
        serde_json::json!({}),
    )
    .to_ticket_payload()
    .unwrap()
    .to_string();
    sqlx::query(
        "INSERT INTO tickets \
         (id, job_id, kind, state, payload, attempt, max_attempts, next_eligible_at, \
          created_at, state_changed_at) VALUES \
         (93, 73, 'synthetic.workflow.operation.identify_media', 'succeeded', ?, 1, 1, \
          '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', '2026-01-01T00:00:10Z')",
    )
    .bind(&decodable)
    .execute(&control.pool)
    .await
    .unwrap();
    // A pre-upgrade row, built to be undecodable for exactly the ADR 0069 reason and
    // no other: a well-formed byte-touching payload with `declared_artifact_access`
    // removed. An envelope-less `'{}'` would also fail to decode, but it failed
    // before this branch too, so it would leave this test green if the declaration
    // gate were later relaxed.
    let mut pre_upgrade =
        crate::workflow::plan::ticket_payload::WorkflowTicketPayload::new_for_test(
            "wf-73",
            "plan-73",
            "node-b",
            "branch-b",
            OperationKind::ProbeFile,
            serde_json::json!({"path": "/library/file-000.mkv"}),
        )
        .to_ticket_payload()
        .unwrap();
    assert!(
        pre_upgrade
            .as_object_mut()
            .unwrap()
            .remove("declared_artifact_access")
            .is_some(),
        "fixture must start from a payload that carries a declaration"
    );
    sqlx::query(
        "INSERT INTO tickets \
         (id, job_id, kind, state, payload, attempt, max_attempts, next_eligible_at, \
          created_at, state_changed_at) VALUES \
         (94, 73, 'synthetic.workflow.operation.probe_file', 'succeeded', ?, 1, 1, \
          '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', '2026-01-01T00:00:10Z')",
    )
    .bind(pre_upgrade.to_string())
    .execute(&control.pool)
    .await
    .unwrap();

    let mut summary = WorkflowRunSummary::empty(JobId(73), Duration::from_secs(3));
    summary
        .refresh_counts(
            &control.tickets,
            &control.leases,
            JobId(73),
            Duration::from_secs(3),
        )
        .await
        .unwrap();

    assert_eq!(summary.ticket_count, 2, "the raw row count includes both");
    assert_eq!(
        summary
            .per_operation
            .get(&OperationKind::IdentifyMedia)
            .map(|operation| operation.ticket_count),
        Some(1),
        "only the decodable ticket reaches the per-operation total"
    );
    // The assertion that binds the skip to its cause. Without it the test passes
    // whether or not the declaration gate still rejects the row, because a decodable
    // probe_file would land in its own per-operation entry and leave the two above
    // untouched.
    assert!(
        !summary
            .per_operation
            .contains_key(&OperationKind::ProbeFile),
        "the undecodable byte-touching ticket reached a per-operation total, so the \
         ADR 0069 declaration gate stopped rejecting a payload with no declaration"
    );
}

#[test]
fn merge_invocation_accumulates_per_operation_successes_across_phases() {
    // Issue #545: the phase-barrier coordinator merges one run summary per
    // phase invocation, so `per_operation` is accumulated, never replaced. The
    // field-by-field asymmetry is deliberate and load-bearing: `dispatch_count`
    // and `success_count` are in-memory per-invocation counters and add, while
    // `ticket_count`, `retry_count` and `failure_count` are recomputed
    // job-cumulatively from durable rows by `refresh_counts`, so merging them
    // takes the maximum instead of double-counting the earlier phase.
    let mut accumulated = WorkflowRunSummary::empty(JobId(1), Duration::from_secs(1));
    accumulated.record_success(OperationKind::Remux);
    accumulated.ticket_count = 1;
    accumulated
        .per_operation
        .entry(OperationKind::Remux)
        .or_default()
        .ticket_count = 1;

    // The transcode phase's own `refresh_counts` sees both phases' durable
    // tickets, so it reports the remux operation again — with a zero success
    // count, because that success belongs to the earlier invocation.
    let mut transcode_phase = WorkflowRunSummary::empty(JobId(1), Duration::from_secs(2));
    transcode_phase.record_success(OperationKind::TranscodeVideo);
    transcode_phase.ticket_count = 2;
    transcode_phase
        .per_operation
        .entry(OperationKind::Remux)
        .or_default()
        .ticket_count = 1;
    transcode_phase
        .per_operation
        .entry(OperationKind::TranscodeVideo)
        .or_default()
        .ticket_count = 1;

    accumulated.merge_invocation(transcode_phase);

    assert_eq!(accumulated.operation_count(OperationKind::Remux), 1);
    assert_eq!(
        accumulated.operation_count(OperationKind::TranscodeVideo),
        1
    );
    assert_eq!(accumulated.ticket_count, 2);
    assert_eq!(
        accumulated.per_operation[&OperationKind::Remux].ticket_count,
        1
    );
}
