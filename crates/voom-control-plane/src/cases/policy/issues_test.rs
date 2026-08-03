use voom_core::{IssueId, IssuePriority, IssueSeverity};
use voom_events::EventKind;
use voom_store::repo::issues::{IssueFilter, IssueStatus};

use crate::ControlPlane;
use crate::cases::{count, cp};

async fn seed_issue(
    cp: &ControlPlane,
    kind: &str,
    severity: &str,
    priority: &str,
    status: &str,
) -> IssueId {
    let ts = "2026-01-01T00:00:00Z";
    let result = sqlx::query(
        "INSERT INTO issues \
         (kind, severity, priority, priority_source, priority_reason, status, \
          title, body, created_at, updated_at) \
         VALUES (?, ?, ?, 'system', 'seed reason', ?, 'seed title', 'seed body', ?, ?)",
    )
    .bind(kind)
    .bind(severity)
    .bind(priority)
    .bind(status)
    .bind(ts)
    .bind(ts)
    .execute(cp.pool_for_test())
    .await
    .unwrap();
    IssueId(u64::try_from(result.last_insert_rowid()).unwrap())
}

#[tokio::test]
async fn list_issues_paginates_by_keyset_and_reports_cursor() {
    let (cp, _tmp) = cp().await;
    let a = seed_issue(&cp, "policy_noncompliant", "medium", "normal", "open").await;
    let b = seed_issue(&cp, "terminal_failure", "high", "high", "open").await;
    let c = seed_issue(&cp, "policy_noncompliant", "low", "low", "planned").await;

    let first = cp
        .list_issues(&IssueFilter::default(), None, 2)
        .await
        .unwrap();
    assert_eq!(
        first.items.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![a, b]
    );
    assert_eq!(first.next_cursor, Some(b.0));

    let second = cp
        .list_issues(&IssueFilter::default(), first.next_cursor, 2)
        .await
        .unwrap();
    assert_eq!(
        second.items.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![c]
    );
    // Non-empty page always reports its last id; the follow-up page is empty.
    assert_eq!(second.next_cursor, Some(c.0));

    let third = cp
        .list_issues(&IssueFilter::default(), second.next_cursor, 2)
        .await
        .unwrap();
    assert!(third.items.is_empty());
    assert_eq!(third.next_cursor, None);
}

#[tokio::test]
async fn list_issues_filters_by_status_kind_and_priority() {
    let (cp, _tmp) = cp().await;
    let _open_policy = seed_issue(&cp, "policy_noncompliant", "medium", "normal", "open").await;
    let planned_terminal = seed_issue(&cp, "terminal_failure", "high", "urgent", "planned").await;

    let by_status = cp
        .list_issues(
            &IssueFilter {
                status: Some(IssueStatus::Planned),
                ..IssueFilter::default()
            },
            None,
            100,
        )
        .await
        .unwrap();
    assert_eq!(
        by_status.items.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![planned_terminal]
    );

    let by_kind = cp
        .list_issues(
            &IssueFilter {
                kind: Some("terminal_failure".to_owned()),
                ..IssueFilter::default()
            },
            None,
            100,
        )
        .await
        .unwrap();
    assert_eq!(by_kind.items.len(), 1);
    assert_eq!(by_kind.items[0].priority, IssuePriority::Urgent);

    let by_priority = cp
        .list_issues(
            &IssueFilter {
                priority: Some(IssuePriority::Normal),
                severity: Some(IssueSeverity::Medium),
                ..IssueFilter::default()
            },
            None,
            100,
        )
        .await
        .unwrap();
    assert_eq!(by_priority.items.len(), 1);
    assert_eq!(by_priority.items[0].kind, "policy_noncompliant");
}

#[tokio::test]
async fn get_issue_returns_record_or_none() {
    let (cp, _tmp) = cp().await;
    let id = seed_issue(&cp, "policy_noncompliant", "medium", "normal", "open").await;

    let found = cp.get_issue(id).await.unwrap().unwrap();
    assert_eq!(found.id, id);
    assert_eq!(found.status, IssueStatus::Open);

    assert!(cp.get_issue(IssueId(999_999)).await.unwrap().is_none());
}

#[tokio::test]
async fn resolve_issue_sets_resolved_and_emits_event() {
    let (cp, _tmp) = cp().await;
    let id = seed_issue(&cp, "terminal_failure", "high", "high", "open").await;

    let resolved = cp.resolve_issue(id).await.unwrap().unwrap();
    assert_eq!(resolved.status, IssueStatus::Resolved);
    assert!(resolved.resolved_at.is_some());
    assert_eq!(resolved.epoch, 1);
    assert_eq!(count(&cp, EventKind::IssueResolved).await, 1);
}

#[tokio::test]
async fn resolve_unknown_issue_is_none_and_emits_nothing() {
    let (cp, _tmp) = cp().await;
    assert!(cp.resolve_issue(IssueId(4242)).await.unwrap().is_none());
    assert_eq!(count(&cp, EventKind::IssueResolved).await, 0);
}

#[tokio::test]
async fn suppress_issue_sets_horizon_and_emits_updated() {
    let (cp, _tmp) = cp().await;
    let id = seed_issue(&cp, "policy_noncompliant", "medium", "normal", "open").await;

    let suppressed = cp.suppress_issue(id, 7).await.unwrap().unwrap();
    assert_eq!(suppressed.status, IssueStatus::Suppressed);
    assert!(suppressed.suppressed_until.is_some());
    assert!(suppressed.resolved_at.is_none());
    assert_eq!(count(&cp, EventKind::IssueUpdated).await, 1);
}

#[tokio::test]
async fn accept_issue_clears_resolved_and_suppressed_bookkeeping() {
    let (cp, _tmp) = cp().await;
    let id = seed_issue(&cp, "policy_noncompliant", "medium", "normal", "open").await;
    // Suppress first so accept has a horizon to clear.
    cp.suppress_issue(id, 3).await.unwrap().unwrap();

    let accepted = cp.accept_issue(id).await.unwrap().unwrap();
    assert_eq!(accepted.status, IssueStatus::Accepted);
    assert!(accepted.suppressed_until.is_none());
    assert!(accepted.resolved_at.is_none());
    // Suppress + accept both emit issue.updated.
    assert_eq!(count(&cp, EventKind::IssueUpdated).await, 2);
}

#[tokio::test]
async fn update_priority_stamps_user_source_and_reason() {
    let (cp, _tmp) = cp().await;
    let id = seed_issue(&cp, "terminal_failure", "high", "normal", "open").await;

    let updated = cp
        .update_issue_priority(
            id,
            IssuePriority::Urgent,
            Some("operator escalation".to_owned()),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(updated.priority, IssuePriority::Urgent);
    assert_eq!(updated.priority_source, "user");
    assert_eq!(
        updated.priority_reason.as_deref(),
        Some("operator escalation")
    );
    // Status is untouched by a priority override.
    assert_eq!(updated.status, IssueStatus::Open);
    assert_eq!(count(&cp, EventKind::IssueUpdated).await, 1);
}
