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
