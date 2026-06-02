use sqlx::{Row, SqlitePool};
use time::OffsetDateTime;
use voom_core::{IssueId, VoomError};

use super::Repository;
use super::common::{i64_from_u64, iso8601, u64_from_i64};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyIssueDraft {
    pub dedupe_key: String,
    pub status: PolicyIssueStatus,
    pub title: String,
    pub body: String,
    pub priority_reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyIssueStatus {
    Open,
    Planned,
    Resolved,
}

impl PolicyIssueStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Planned => "planned",
            Self::Resolved => "resolved",
        }
    }

    fn parse(s: &str) -> Result<Self, VoomError> {
        match s {
            "open" => Ok(Self::Open),
            "planned" => Ok(Self::Planned),
            "resolved" => Ok(Self::Resolved),
            other => Err(VoomError::Database(format!(
                "issues.status {other:?} not in policy issue vocab"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyIssueRow {
    pub id: IssueId,
    pub dedupe_key: String,
    pub status: PolicyIssueStatus,
    pub epoch: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyIssueMutationKind {
    Created,
    Updated,
    Resolved,
    Unchanged,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyIssueMutation {
    pub kind: PolicyIssueMutationKind,
    pub row: PolicyIssueRow,
}

#[derive(Debug, Clone)]
pub struct SqliteIssueRepo;

impl SqliteIssueRepo {
    #[must_use]
    pub fn new(_pool: SqlitePool) -> Self {
        Self
    }
}

impl Repository for SqliteIssueRepo {}

impl SqliteIssueRepo {
    pub async fn upsert_policy_noncompliant_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        draft: PolicyIssueDraft,
        now: OffsetDateTime,
    ) -> Result<PolicyIssueMutation, VoomError> {
        let timestamp = iso8601(now)?;
        let inserted = sqlx::query(
            "INSERT INTO issues \
             (kind, severity, priority, priority_source, priority_reason, status, \
              suppressed_until, title, body, created_at, updated_at, resolved_at, dedupe_key) \
             VALUES ('policy_noncompliant', 'medium', 'normal', 'policy', ?, ?, \
                     NULL, ?, ?, ?, ?, NULL, ?)",
        )
        .bind(&draft.priority_reason)
        .bind(draft.status.as_str())
        .bind(&draft.title)
        .bind(&draft.body)
        .bind(&timestamp)
        .bind(&timestamp)
        .bind(&draft.dedupe_key)
        .execute(&mut **tx)
        .await;

        let existing = match inserted {
            Ok(result) => {
                return Ok(PolicyIssueMutation {
                    kind: PolicyIssueMutationKind::Created,
                    row: PolicyIssueRow {
                        id: IssueId(u64_from_i64(result.last_insert_rowid())),
                        dedupe_key: draft.dedupe_key,
                        status: draft.status,
                        epoch: 0,
                    },
                });
            }
            Err(err) => {
                let existing = select_issue_detail(tx, &draft.dedupe_key).await?;
                let Some(existing) = existing else {
                    return Err(VoomError::Database(format!("issues insert: {err}")));
                };
                existing
            }
        };
        if existing.row.status == draft.status
            && existing.title == draft.title
            && existing.body == draft.body
            && existing.priority_reason.as_deref() == Some(draft.priority_reason.as_str())
        {
            return Ok(PolicyIssueMutation {
                kind: PolicyIssueMutationKind::Unchanged,
                row: existing.row,
            });
        }

        sqlx::query(
            "UPDATE issues \
             SET status = ?, title = ?, body = ?, priority_reason = ?, updated_at = ?, \
                 resolved_at = NULL, epoch = epoch + 1 \
             WHERE id = ?",
        )
        .bind(draft.status.as_str())
        .bind(&draft.title)
        .bind(&draft.body)
        .bind(&draft.priority_reason)
        .bind(&timestamp)
        .bind(i64_from_u64(existing.row.id.0))
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("issues update: {e}")))?;

        Ok(PolicyIssueMutation {
            kind: PolicyIssueMutationKind::Updated,
            row: PolicyIssueRow {
                id: existing.row.id,
                dedupe_key: existing.row.dedupe_key,
                status: draft.status,
                epoch: existing.row.epoch + 1,
            },
        })
    }

    pub async fn resolve_policy_noncompliant_by_dedupe_key_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        dedupe_key: &str,
        title: &str,
        body: &str,
        now: OffsetDateTime,
    ) -> Result<Option<PolicyIssueMutation>, VoomError> {
        let Some(existing) = select_live_policy_issue(tx, dedupe_key).await? else {
            return Ok(None);
        };
        let timestamp = iso8601(now)?;
        sqlx::query(
            "UPDATE issues \
             SET status = 'resolved', title = ?, body = ?, updated_at = ?, resolved_at = ?, \
                 epoch = epoch + 1 \
             WHERE id = ?",
        )
        .bind(title)
        .bind(body)
        .bind(&timestamp)
        .bind(&timestamp)
        .bind(i64_from_u64(existing.id.0))
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("issues resolve: {e}")))?;

        Ok(Some(PolicyIssueMutation {
            kind: PolicyIssueMutationKind::Resolved,
            row: PolicyIssueRow {
                status: PolicyIssueStatus::Resolved,
                epoch: existing.epoch + 1,
                ..existing
            },
        }))
    }

    pub async fn list_live_policy_noncompliant_by_dedupe_prefix_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        dedupe_prefix: &str,
    ) -> Result<Vec<PolicyIssueRow>, VoomError> {
        let rows = sqlx::query(
            "SELECT id, dedupe_key, status, epoch \
             FROM issues \
             WHERE kind = 'policy_noncompliant' \
               AND status IN ('open', 'planned') \
               AND dedupe_key LIKE ? ESCAPE '\\' \
             ORDER BY id ASC",
        )
        .bind(dedupe_prefix)
        .fetch_all(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("issues list live: {e}")))?;

        rows.iter().map(row_to_policy_issue).collect()
    }
}

#[derive(Debug)]
struct PolicyIssueDetail {
    row: PolicyIssueRow,
    title: String,
    body: String,
    priority_reason: Option<String>,
}

async fn select_issue_detail(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    dedupe_key: &str,
) -> Result<Option<PolicyIssueDetail>, VoomError> {
    let row = sqlx::query(
        "SELECT id, dedupe_key, status, epoch, title, body, priority_reason \
         FROM issues WHERE kind = 'policy_noncompliant' AND dedupe_key = ?",
    )
    .bind(dedupe_key)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|e| VoomError::Database(format!("issues select detail: {e}")))?;
    row.as_ref().map(row_to_policy_issue_detail).transpose()
}

async fn select_live_policy_issue(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    dedupe_key: &str,
) -> Result<Option<PolicyIssueRow>, VoomError> {
    let row = sqlx::query(
        "SELECT id, dedupe_key, status, epoch \
         FROM issues \
         WHERE kind = 'policy_noncompliant' \
           AND dedupe_key = ? \
           AND status IN ('open', 'planned')",
    )
    .bind(dedupe_key)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|e| VoomError::Database(format!("issues select live: {e}")))?;
    row.as_ref().map(row_to_policy_issue).transpose()
}

fn row_to_policy_issue(row: &sqlx::sqlite::SqliteRow) -> Result<PolicyIssueRow, VoomError> {
    let id: i64 = row
        .try_get("id")
        .map_err(|e| VoomError::Database(format!("read issue id: {e}")))?;
    let dedupe_key: String = row
        .try_get("dedupe_key")
        .map_err(|e| VoomError::Database(format!("read issue dedupe_key: {e}")))?;
    let status: String = row
        .try_get("status")
        .map_err(|e| VoomError::Database(format!("read issue status: {e}")))?;
    let epoch: i64 = row
        .try_get("epoch")
        .map_err(|e| VoomError::Database(format!("read issue epoch: {e}")))?;
    Ok(PolicyIssueRow {
        id: IssueId(u64_from_i64(id)),
        dedupe_key,
        status: PolicyIssueStatus::parse(&status)?,
        epoch: u64_from_i64(epoch),
    })
}

fn row_to_policy_issue_detail(
    row: &sqlx::sqlite::SqliteRow,
) -> Result<PolicyIssueDetail, VoomError> {
    let detail = PolicyIssueDetail {
        row: row_to_policy_issue(row)?,
        title: row
            .try_get("title")
            .map_err(|e| VoomError::Database(format!("read issue title: {e}")))?,
        body: row
            .try_get("body")
            .map_err(|e| VoomError::Database(format!("read issue body: {e}")))?,
        priority_reason: row
            .try_get("priority_reason")
            .map_err(|e| VoomError::Database(format!("read issue priority_reason: {e}")))?,
    };
    Ok(detail)
}

#[cfg(test)]
#[path = "issues_test.rs"]
mod tests;
