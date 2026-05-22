use async_trait::async_trait;
use serde_json::Value as JsonValue;
use sqlx::{Row, SqlitePool};
use time::OffsetDateTime;
use voom_core::{PolicyDocumentId, PolicyVersionId, VoomError};

use super::Repository;
use super::common::{
    i64_from_u64, iso8601, map_row_err, parse_iso8601, serialize_json, u32_from_i64, u64_from_i64,
};

#[derive(Debug, Clone, PartialEq)]
pub struct PolicyDocument {
    pub id: PolicyDocumentId,
    pub slug: String,
    pub display_name: String,
    pub created_at: OffsetDateTime,
    pub current_accepted_version_id: Option<PolicyVersionId>,
    pub epoch: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PolicyVersion {
    pub id: PolicyVersionId,
    pub policy_document_id: PolicyDocumentId,
    pub version_number: u64,
    pub source_text: String,
    pub source_hash: String,
    pub schema_version: u32,
    pub compiled_json: JsonValue,
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NewPolicyDocumentVersion {
    pub slug: String,
    pub display_name: Option<String>,
    pub source_text: String,
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CreatedPolicyVersion {
    pub document: PolicyDocument,
    pub version: PolicyVersion,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyDocumentSummary {
    pub id: PolicyDocumentId,
    pub slug: String,
    pub display_name: String,
    pub created_at: OffsetDateTime,
    pub current_accepted_version_id: Option<PolicyVersionId>,
    pub epoch: u64,
}

#[async_trait]
pub trait PolicyRepo: Repository {
    async fn create_document_with_version(
        &self,
        draft: NewPolicyDocumentVersion,
    ) -> Result<CreatedPolicyVersion, VoomError>;

    async fn add_version(
        &self,
        document_id: PolicyDocumentId,
        source_text: String,
    ) -> Result<PolicyVersion, VoomError>;

    async fn get_document(&self, id: PolicyDocumentId)
    -> Result<Option<PolicyDocument>, VoomError>;
    async fn list_documents(&self) -> Result<Vec<PolicyDocumentSummary>, VoomError>;
    async fn get_version(&self, id: PolicyVersionId) -> Result<Option<PolicyVersion>, VoomError>;
    async fn list_versions(
        &self,
        document_id: PolicyDocumentId,
    ) -> Result<Vec<PolicyVersion>, VoomError>;
}

#[derive(Debug, Clone)]
pub struct SqlitePolicyRepo {
    pool: SqlitePool,
}

impl SqlitePolicyRepo {
    #[must_use]
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

impl Repository for SqlitePolicyRepo {}

#[async_trait]
impl PolicyRepo for SqlitePolicyRepo {
    async fn create_document_with_version(
        &self,
        draft: NewPolicyDocumentVersion,
    ) -> Result<CreatedPolicyVersion, VoomError> {
        validate_slug(&draft.slug)?;
        let compiled = voom_policy::compile_policy(&draft.source_text).map_err(|err| err.error)?;
        let compiled_json = voom_policy::deterministic_json(&compiled.policy)?;
        let compiled_json_text = serialize_json(&compiled_json, "policy_versions.compiled_json")?;
        let created_at = iso8601(draft.created_at)?;
        let display_name = draft
            .display_name
            .unwrap_or_else(|| compiled.policy.policy_name.clone());

        let mut tx = begin_immediate(&self.pool).await?;
        let document_res = sqlx::query(
            "INSERT INTO policy_documents (slug, display_name, created_at) VALUES (?, ?, ?)",
        )
        .bind(&draft.slug)
        .bind(&display_name)
        .bind(&created_at)
        .execute(&mut *tx)
        .await
        .map_err(|e| VoomError::Database(format!("policy_documents insert: {e}")))?;
        let document_id = PolicyDocumentId(u64_from_i64(document_res.last_insert_rowid()));

        let version_res = insert_version(
            &mut tx,
            NewVersionRow {
                document_id,
                version_number: 1,
                source_text: &draft.source_text,
                source_hash: &compiled.policy.source_hash,
                schema_version: compiled.policy.schema_version,
                compiled_json_text: &compiled_json_text,
                created_at: &created_at,
            },
        )
        .await?;
        let version_id = PolicyVersionId(u64_from_i64(version_res.last_insert_rowid()));
        advance_current_version(&mut tx, document_id, version_id).await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::Database(format!("policy registry commit: {e}")))?;

        let document = self.get_document(document_id).await?.ok_or_else(|| {
            VoomError::Internal(format!("created policy document {document_id} missing"))
        })?;
        let version = self.get_version(version_id).await?.ok_or_else(|| {
            VoomError::Internal(format!("created policy version {version_id} missing"))
        })?;
        Ok(CreatedPolicyVersion { document, version })
    }

    async fn add_version(
        &self,
        document_id: PolicyDocumentId,
        source_text: String,
    ) -> Result<PolicyVersion, VoomError> {
        let source_hash = voom_policy::source_hash(&source_text);
        if let Some(existing) = self
            .get_version_by_document_and_hash(document_id, &source_hash)
            .await?
        {
            return Ok(existing);
        }

        let compiled = voom_policy::compile_policy(&source_text).map_err(|err| err.error)?;
        let compiled_json = voom_policy::deterministic_json(&compiled.policy)?;
        let compiled_json_text = serialize_json(&compiled_json, "policy_versions.compiled_json")?;
        let created_at = iso8601(OffsetDateTime::now_utc())?;

        let mut tx = begin_immediate(&self.pool).await?;
        if let Some(existing) =
            get_version_by_document_and_hash_in_tx(&mut tx, document_id, &source_hash).await?
        {
            tx.commit()
                .await
                .map_err(|e| VoomError::Database(format!("policy registry commit: {e}")))?;
            return Ok(existing);
        }

        let version_number = next_version_number(&mut tx, document_id).await?;
        let version_res = match insert_version(
            &mut tx,
            NewVersionRow {
                document_id,
                version_number,
                source_text: &source_text,
                source_hash: &source_hash,
                schema_version: compiled.policy.schema_version,
                compiled_json_text: &compiled_json_text,
                created_at: &created_at,
            },
        )
        .await
        {
            Ok(res) => res,
            Err(err) => {
                let _ = tx.rollback().await;
                if let Some(existing) = self
                    .get_version_by_document_and_hash(document_id, &source_hash)
                    .await?
                {
                    return Ok(existing);
                }
                return Err(err);
            }
        };
        let version_id = PolicyVersionId(u64_from_i64(version_res.last_insert_rowid()));
        advance_current_version(&mut tx, document_id, version_id).await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::Database(format!("policy registry commit: {e}")))?;

        self.get_version(version_id).await?.ok_or_else(|| {
            VoomError::Internal(format!("created policy version {version_id} missing"))
        })
    }

    async fn get_document(
        &self,
        id: PolicyDocumentId,
    ) -> Result<Option<PolicyDocument>, VoomError> {
        let row = sqlx::query(DOCUMENT_SELECT_BY_ID)
            .bind(i64_from_u64(id.0))
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| VoomError::Database(format!("policy_documents get: {e}")))?;
        row.as_ref().map(row_to_document).transpose()
    }

    async fn list_documents(&self) -> Result<Vec<PolicyDocumentSummary>, VoomError> {
        let rows = sqlx::query(
            "SELECT id, slug, display_name, created_at, current_accepted_version_id, epoch \
             FROM policy_documents ORDER BY slug ASC, id ASC",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| VoomError::Database(format!("policy_documents list: {e}")))?;

        rows.iter()
            .map(row_to_document_summary)
            .collect::<Result<Vec<_>, _>>()
    }

    async fn get_version(&self, id: PolicyVersionId) -> Result<Option<PolicyVersion>, VoomError> {
        let row = sqlx::query(VERSION_SELECT_BY_ID)
            .bind(i64_from_u64(id.0))
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| VoomError::Database(format!("policy_versions get: {e}")))?;
        row.as_ref().map(row_to_version).transpose()
    }

    async fn list_versions(
        &self,
        document_id: PolicyDocumentId,
    ) -> Result<Vec<PolicyVersion>, VoomError> {
        let rows = sqlx::query(
            "SELECT id, policy_document_id, version_number, source_text, source_hash, \
                    schema_version, compiled_json, created_at \
             FROM policy_versions WHERE policy_document_id = ? \
             ORDER BY version_number ASC, id ASC",
        )
        .bind(i64_from_u64(document_id.0))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| VoomError::Database(format!("policy_versions list: {e}")))?;

        rows.iter()
            .map(row_to_version)
            .collect::<Result<Vec<_>, _>>()
    }
}

impl SqlitePolicyRepo {
    async fn get_version_by_document_and_hash(
        &self,
        document_id: PolicyDocumentId,
        source_hash: &str,
    ) -> Result<Option<PolicyVersion>, VoomError> {
        let row = sqlx::query(VERSION_SELECT_BY_DOCUMENT_AND_HASH)
            .bind(i64_from_u64(document_id.0))
            .bind(source_hash)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| VoomError::Database(format!("policy_versions get by hash: {e}")))?;
        row.as_ref().map(row_to_version).transpose()
    }
}

const DOCUMENT_SELECT_BY_ID: &str = "SELECT id, slug, display_name, created_at, \
    current_accepted_version_id, epoch FROM policy_documents WHERE id = ?";

const VERSION_SELECT_BY_ID: &str = "SELECT id, policy_document_id, version_number, source_text, \
    source_hash, schema_version, compiled_json, created_at FROM policy_versions WHERE id = ?";

const VERSION_SELECT_BY_DOCUMENT_AND_HASH: &str = "SELECT id, policy_document_id, \
    version_number, source_text, source_hash, schema_version, compiled_json, created_at \
    FROM policy_versions WHERE policy_document_id = ? AND source_hash = ?";

async fn begin_immediate(
    pool: &SqlitePool,
) -> Result<sqlx::Transaction<'static, sqlx::Sqlite>, VoomError> {
    pool.begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(|e| VoomError::Database(format!("policy registry begin IMMEDIATE: {e}")))
}

struct NewVersionRow<'a> {
    document_id: PolicyDocumentId,
    version_number: u64,
    source_text: &'a str,
    source_hash: &'a str,
    schema_version: u32,
    compiled_json_text: &'a str,
    created_at: &'a str,
}

async fn insert_version(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    row: NewVersionRow<'_>,
) -> Result<sqlx::sqlite::SqliteQueryResult, VoomError> {
    sqlx::query(
        "INSERT INTO policy_versions \
         (policy_document_id, version_number, source_text, source_hash, schema_version, \
         compiled_json, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(i64_from_u64(row.document_id.0))
    .bind(i64_from_u64(row.version_number))
    .bind(row.source_text)
    .bind(row.source_hash)
    .bind(i64::from(row.schema_version))
    .bind(row.compiled_json_text)
    .bind(row.created_at)
    .execute(&mut **tx)
    .await
    .map_err(|e| {
        if is_unique_violation(&e) {
            VoomError::Conflict(format!("policy version conflict: {e}"))
        } else {
            VoomError::Database(format!("policy_versions insert: {e}"))
        }
    })
}

async fn advance_current_version(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    document_id: PolicyDocumentId,
    version_id: PolicyVersionId,
) -> Result<(), VoomError> {
    let result = sqlx::query(
        "UPDATE policy_documents \
         SET current_accepted_version_id = ?, epoch = epoch + 1 \
         WHERE id = ?",
    )
    .bind(i64_from_u64(version_id.0))
    .bind(i64_from_u64(document_id.0))
    .execute(&mut **tx)
    .await
    .map_err(|e| VoomError::Database(format!("policy_documents advance current version: {e}")))?;
    if result.rows_affected() == 0 {
        return Err(VoomError::NotFound(format!(
            "policy document {document_id} not found"
        )));
    }
    Ok(())
}

async fn next_version_number(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    document_id: PolicyDocumentId,
) -> Result<u64, VoomError> {
    let next: Option<i64> = sqlx::query_scalar(
        "SELECT MAX(version_number) + 1 FROM policy_versions WHERE policy_document_id = ?",
    )
    .bind(i64_from_u64(document_id.0))
    .fetch_one(&mut **tx)
    .await
    .map_err(|e| VoomError::Database(format!("policy_versions next version: {e}")))?;
    let next = next
        .ok_or_else(|| VoomError::NotFound(format!("policy document {document_id} not found")))?;
    Ok(u64_from_i64(next))
}

async fn get_version_by_document_and_hash_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    document_id: PolicyDocumentId,
    source_hash: &str,
) -> Result<Option<PolicyVersion>, VoomError> {
    let row = sqlx::query(VERSION_SELECT_BY_DOCUMENT_AND_HASH)
        .bind(i64_from_u64(document_id.0))
        .bind(source_hash)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("policy_versions get by hash: {e}")))?;
    row.as_ref().map(row_to_version).transpose()
}

fn validate_slug(slug: &str) -> Result<(), VoomError> {
    if is_stable_token(slug) {
        Ok(())
    } else {
        Err(VoomError::Config(format!(
            "policy document slug must be a stable token: {slug:?}"
        )))
    }
}

fn is_stable_token(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

fn is_unique_violation(err: &sqlx::Error) -> bool {
    match err {
        sqlx::Error::Database(db_err) => db_err.is_unique_violation(),
        _ => false,
    }
}

fn row_to_document(row: &sqlx::sqlite::SqliteRow) -> Result<PolicyDocument, VoomError> {
    let id: i64 = row
        .try_get("id")
        .map_err(|e| map_row_err("policy_documents", &e))?;
    let current_accepted_version_id: Option<i64> = row
        .try_get("current_accepted_version_id")
        .map_err(|e| map_row_err("policy_documents", &e))?;
    let created_at: String = row
        .try_get("created_at")
        .map_err(|e| map_row_err("policy_documents", &e))?;
    let epoch: i64 = row
        .try_get("epoch")
        .map_err(|e| map_row_err("policy_documents", &e))?;

    Ok(PolicyDocument {
        id: PolicyDocumentId(u64_from_i64(id)),
        slug: row
            .try_get("slug")
            .map_err(|e| map_row_err("policy_documents", &e))?,
        display_name: row
            .try_get("display_name")
            .map_err(|e| map_row_err("policy_documents", &e))?,
        created_at: parse_iso8601(&created_at)?,
        current_accepted_version_id: current_accepted_version_id
            .map(u64_from_i64)
            .map(PolicyVersionId),
        epoch: u64_from_i64(epoch),
    })
}

fn row_to_document_summary(
    row: &sqlx::sqlite::SqliteRow,
) -> Result<PolicyDocumentSummary, VoomError> {
    let document = row_to_document(row)?;
    Ok(PolicyDocumentSummary {
        id: document.id,
        slug: document.slug,
        display_name: document.display_name,
        created_at: document.created_at,
        current_accepted_version_id: document.current_accepted_version_id,
        epoch: document.epoch,
    })
}

fn row_to_version(row: &sqlx::sqlite::SqliteRow) -> Result<PolicyVersion, VoomError> {
    let id: i64 = row
        .try_get("id")
        .map_err(|e| map_row_err("policy_versions", &e))?;
    let document_id: i64 = row
        .try_get("policy_document_id")
        .map_err(|e| map_row_err("policy_versions", &e))?;
    let version_number: i64 = row
        .try_get("version_number")
        .map_err(|e| map_row_err("policy_versions", &e))?;
    let schema_version: i64 = row
        .try_get("schema_version")
        .map_err(|e| map_row_err("policy_versions", &e))?;
    let compiled_json_text: String = row
        .try_get("compiled_json")
        .map_err(|e| map_row_err("policy_versions", &e))?;
    let compiled_json = serde_json::from_str(&compiled_json_text)
        .map_err(|e| VoomError::Database(format!("policy_versions.compiled_json parse: {e}")))?;
    let created_at: String = row
        .try_get("created_at")
        .map_err(|e| map_row_err("policy_versions", &e))?;

    Ok(PolicyVersion {
        id: PolicyVersionId(u64_from_i64(id)),
        policy_document_id: PolicyDocumentId(u64_from_i64(document_id)),
        version_number: u64_from_i64(version_number),
        source_text: row
            .try_get("source_text")
            .map_err(|e| map_row_err("policy_versions", &e))?,
        source_hash: row
            .try_get("source_hash")
            .map_err(|e| map_row_err("policy_versions", &e))?,
        schema_version: u32_from_i64(schema_version)?,
        compiled_json,
        created_at: parse_iso8601(&created_at)?,
    })
}

#[cfg(test)]
#[path = "policies_test.rs"]
mod tests;
