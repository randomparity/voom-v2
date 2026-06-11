//! `EventRepo` — the single write path into the `events` table.
//! Read methods (`list`, `tail`, `get`) serve the M3 inspection CLI.

use std::fmt::Write as _;

use async_trait::async_trait;
use serde_json::Value as JsonValue;
use sqlx::{Row, SqlitePool};
use voom_core::VoomError;
use voom_events::{Event, EventEnvelope, EventId, EventKind, SubjectType, TraceId};

use super::Repository;
use super::common::{i64_from_u64, iso8601, parse_iso8601, u64_from_i64};

#[derive(Debug, Clone, Default)]
pub struct EventFilter {
    pub kind: Option<EventKind>,
    pub subject_type: Option<SubjectType>,
    pub subject_id: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct Page {
    pub limit: u32,
    pub cursor: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct EventRow {
    pub id: EventId,
    pub envelope: EventEnvelope,
}

#[derive(Debug, Clone)]
pub struct EventPage {
    pub items: Vec<EventRow>,
    pub next_cursor: Option<u64>,
}

#[async_trait]
pub trait EventRepo: Repository {
    async fn append_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        envelope: EventEnvelope,
    ) -> Result<EventId, VoomError>;

    async fn list(&self, filter: EventFilter, page: Page) -> Result<EventPage, VoomError>;
    async fn tail(&self, filter: EventFilter, page: Page) -> Result<EventPage, VoomError>;
    async fn get(&self, event_id: EventId) -> Result<Option<EventRow>, VoomError>;
}

#[derive(Debug, Clone)]
pub struct SqliteEventRepo {
    pool: SqlitePool,
}

impl SqliteEventRepo {
    #[must_use]
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

impl Repository for SqliteEventRepo {}

#[async_trait]
impl EventRepo for SqliteEventRepo {
    async fn append_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        env: EventEnvelope,
    ) -> Result<EventId, VoomError> {
        // The on-disk payload is the typed `Event`'s JSON. We strip the
        // kind/subject envelope back out, since they live in dedicated
        // columns.
        let payload_json = serde_json::to_string(&inner_payload(&env.payload))
            .map_err(|e| VoomError::Internal(format!("payload serialize: {e}")))?;
        let occurred = iso8601(env.occurred_at)?;
        let res = sqlx::query(
            "INSERT INTO events (occurred_at, kind, subject_type, subject_id, trace_id, payload) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(occurred)
        .bind(env.payload.kind().as_str())
        .bind(env.subject_type.as_str())
        .bind(env.subject_id.map(i64_from_u64))
        .bind(env.trace_id.as_ref().map(|t| t.0.clone()))
        .bind(payload_json)
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("events append", e))?;
        Ok(EventId(u64_from_i64(res.last_insert_rowid())))
    }

    async fn list(&self, filter: EventFilter, page: Page) -> Result<EventPage, VoomError> {
        page_query(&self.pool, filter, page, /* tail = */ false).await
    }

    async fn tail(&self, filter: EventFilter, page: Page) -> Result<EventPage, VoomError> {
        page_query(&self.pool, filter, page, /* tail = */ true).await
    }

    async fn get(&self, event_id: EventId) -> Result<Option<EventRow>, VoomError> {
        let row = sqlx::query(
            "SELECT event_id, occurred_at, kind, subject_type, subject_id, trace_id, payload \
             FROM events WHERE event_id = ?",
        )
        .bind(i64_from_u64(event_id.0))
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| VoomError::database_context("events get", e))?;
        // An unknown-kind row yields Ok(None): the caller cannot represent it,
        // which surfaces the same as "not found" rather than erroring.
        match row {
            Some(row) => row_to_event(&row),
            None => Ok(None),
        }
    }
}

async fn page_query(
    pool: &SqlitePool,
    filter: EventFilter,
    page: Page,
    tail: bool,
) -> Result<EventPage, VoomError> {
    // Build dynamic SQL: WHERE filters + ordering + LIMIT + cursor.
    let order = if tail { "DESC" } else { "ASC" };
    let mut sql = String::from(
        "SELECT event_id, occurred_at, kind, subject_type, subject_id, trace_id, payload \
         FROM events WHERE 1=1",
    );
    if filter.kind.is_some() {
        sql.push_str(" AND kind = ?");
    }
    if filter.subject_type.is_some() {
        sql.push_str(" AND subject_type = ?");
    }
    if filter.subject_id.is_some() {
        sql.push_str(" AND subject_id = ?");
    }
    if page.cursor.is_some() {
        sql.push_str(if tail {
            " AND event_id < ?"
        } else {
            " AND event_id > ?"
        });
    }
    write!(sql, " ORDER BY event_id {order} LIMIT ?")
        .map_err(|e| VoomError::Internal(format!("build list SQL: {e}")))?;

    let mut q = sqlx::query(&sql);
    if let Some(k) = filter.kind {
        q = q.bind(k.as_str());
    }
    if let Some(s) = filter.subject_type {
        q = q.bind(s.as_str());
    }
    if let Some(s) = filter.subject_id {
        q = q.bind(i64_from_u64(s));
    }
    if let Some(c) = page.cursor {
        q = q.bind(i64_from_u64(c));
    }
    q = q.bind(i64::from(page.limit));

    let rows = q
        .fetch_all(pool)
        .await
        .map_err(|e| VoomError::database_context("events list", e))?;

    // Rows with an unknown `kind` (written by a newer binary) are skipped so a
    // single unrecognized row does not poison the whole read. The cursor must
    // still advance past skipped rows — keyed off the last RAW row id, not the
    // last kept item — or `tail` would re-scan a trailing run of unknown rows.
    let mut items = Vec::with_capacity(rows.len());
    let mut last_raw_id: Option<u64> = None;
    for row in &rows {
        last_raw_id = Some(event_row_id(row)?);
        if let Some(event) = row_to_event(row)? {
            items.push(event);
        }
    }

    // Forward `list` must signal exhaustion: an empty page returns None so
    // pollers stop. `tail` (live polling) keeps the caller's cursor alive
    // across empty pages so a follower can resume when new events arrive.
    let next_cursor = if tail {
        last_raw_id.or(page.cursor)
    } else {
        last_raw_id
    };
    Ok(EventPage { items, next_cursor })
}

fn event_row_id(row: &sqlx::sqlite::SqliteRow) -> Result<u64, VoomError> {
    let id: i64 = row
        .try_get("event_id")
        .map_err(|e| VoomError::database_context("read event_id", e))?;
    Ok(u64_from_i64(id))
}

/// Reconstruct an `EventRow`, or `Ok(None)` if the row's `kind` is not in this
/// build's `EventKind` vocab — i.e. an event written by a newer binary. Such a
/// row is skipped (with a warning) rather than failing the whole read, so an
/// older reader stays usable against a forward-migrated database. Genuine
/// corruption (bad timestamp, malformed payload JSON, unknown `subject_type`)
/// still returns `Err`.
fn row_to_event(row: &sqlx::sqlite::SqliteRow) -> Result<Option<EventRow>, VoomError> {
    let id = event_row_id(row)?;
    let occurred: String = row
        .try_get("occurred_at")
        .map_err(|e| VoomError::database_context("read occurred_at", e))?;
    let kind_str: String = row
        .try_get("kind")
        .map_err(|e| VoomError::database_context("read kind", e))?;
    let subject_type_str: String = row
        .try_get("subject_type")
        .map_err(|e| VoomError::database_context("read subject_type", e))?;
    let subject_id_i64: Option<i64> = row
        .try_get("subject_id")
        .map_err(|e| VoomError::database_context("read subject_id", e))?;
    let trace_id: Option<String> = row
        .try_get("trace_id")
        .map_err(|e| VoomError::database_context("read trace_id", e))?;
    let payload: String = row
        .try_get("payload")
        .map_err(|e| VoomError::database_context("read payload", e))?;

    let occurred_at = parse_iso8601(&occurred)?;
    // Decode via the explicit string → enum parsers. Using serde derives
    // would produce snake_case strings that don't match the dotted wire
    // form `as_str()` writes; see `EventKind` rustdoc.
    // `from_str` fails only for a kind outside the vocab; treat that as a
    // forward-compat unknown and skip rather than erroring the read.
    let Ok(kind) = EventKind::from_str(&kind_str) else {
        tracing::warn!(
            event_id = id,
            kind = kind_str,
            "skipping event with unknown kind (written by a newer binary?)"
        );
        return Ok(None);
    };
    let subject_type = SubjectType::from_str(&subject_type_str)?;
    let payload_value: JsonValue = serde_json::from_str(&payload)
        .map_err(|e| VoomError::database_context("parse payload JSON", e))?;
    let event = reassemble_event(kind, &payload_value)?;
    Ok(Some(EventRow {
        id: EventId(id),
        envelope: EventEnvelope {
            occurred_at,
            subject_type,
            subject_id: subject_id_i64.map(u64_from_i64),
            trace_id: trace_id.map(TraceId),
            payload: event,
        },
    }))
}

fn inner_payload(event: &Event) -> JsonValue {
    // Strip the tag wrapper: we store the inner payload only since `kind`
    // already lives in its own column.
    let v = serde_json::to_value(event).unwrap_or(JsonValue::Null);
    if let JsonValue::Object(map) = &v
        && let Some(p) = map.get("payload")
    {
        return p.clone();
    }
    v
}

fn reassemble_event(kind: EventKind, payload: &JsonValue) -> Result<Event, VoomError> {
    // Build the serde-tagged wire shape using the dotted-form string
    // directly. `EventKind` does not derive `Serialize`, and the `Event`
    // sum type's per-variant `#[serde(rename = "...")]` is the dotted
    // form — keeping this explicit ensures the on-disk `kind` column
    // value is what `Event` deserializes against.
    let tagged = serde_json::json!({ "kind": kind.as_str(), "payload": payload });
    serde_json::from_value::<Event>(tagged)
        .map_err(|e| VoomError::database_context(format!("rebuild Event for {kind:?}"), e))
}

#[cfg(test)]
#[path = "events_test.rs"]
mod tests;
