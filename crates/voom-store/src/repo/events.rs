//! `EventRepo` — the single write path into the `events` table.
//! Read methods (`list`, `tail`, `get`) serve the M3 inspection CLI.

use std::fmt::Write as _;

use async_trait::async_trait;
use serde_json::Value as JsonValue;
use sqlx::{Row, SqlitePool};
use time::OffsetDateTime;
use voom_core::VoomError;
use voom_events::{Event, EventEnvelope, EventId, EventKind, SubjectType, TraceId};

use super::Repository;

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
        let occurred = env
            .occurred_at
            .format(&time::format_description::well_known::Iso8601::DEFAULT)
            .map_err(|e| VoomError::Internal(format!("format occurred_at: {e}")))?;
        let res = sqlx::query(
            "INSERT INTO events (occurred_at, kind, subject_type, subject_id, trace_id, payload) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(occurred)
        .bind(env.kind.as_str())
        .bind(env.subject_type.as_str())
        .bind(env.subject_id.map(i64_from_u64))
        .bind(env.trace_id.as_ref().map(|t| t.0.clone()))
        .bind(payload_json)
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("events append: {e}")))?;
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
        .map_err(|e| VoomError::Database(format!("events get: {e}")))?;
        row.as_ref().map(row_to_event).transpose()
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
        .map_err(|e| VoomError::Database(format!("events list: {e}")))?;
    let items: Vec<EventRow> = rows.iter().map(row_to_event).collect::<Result<_, _>>()?;

    let next_cursor = items.last().map(|r| r.id.0).or(page.cursor);
    Ok(EventPage { items, next_cursor })
}

fn row_to_event(row: &sqlx::sqlite::SqliteRow) -> Result<EventRow, VoomError> {
    let id: i64 = row
        .try_get("event_id")
        .map_err(|e| VoomError::Database(format!("read event_id: {e}")))?;
    let occurred: String = row
        .try_get("occurred_at")
        .map_err(|e| VoomError::Database(format!("read occurred_at: {e}")))?;
    let kind_str: String = row
        .try_get("kind")
        .map_err(|e| VoomError::Database(format!("read kind: {e}")))?;
    let subject_type_str: String = row
        .try_get("subject_type")
        .map_err(|e| VoomError::Database(format!("read subject_type: {e}")))?;
    let subject_id_i64: Option<i64> = row
        .try_get("subject_id")
        .map_err(|e| VoomError::Database(format!("read subject_id: {e}")))?;
    let trace_id: Option<String> = row
        .try_get("trace_id")
        .map_err(|e| VoomError::Database(format!("read trace_id: {e}")))?;
    let payload: String = row
        .try_get("payload")
        .map_err(|e| VoomError::Database(format!("read payload: {e}")))?;

    let occurred_at = OffsetDateTime::parse(
        &occurred,
        &time::format_description::well_known::Iso8601::DEFAULT,
    )
    .map_err(|e| VoomError::Database(format!("parse occurred_at: {e}")))?;
    // Decode via the explicit string → enum parsers. Using serde derives
    // would produce snake_case strings that don't match the dotted wire
    // form `as_str()` writes; see `EventKind` rustdoc.
    let kind = EventKind::from_str(&kind_str)?;
    let subject_type = SubjectType::from_str(&subject_type_str)?;
    let payload_value: JsonValue = serde_json::from_str(&payload)
        .map_err(|e| VoomError::Database(format!("parse payload JSON: {e}")))?;
    let event = reassemble_event(kind, &payload_value)?;
    Ok(EventRow {
        id: EventId(u64_from_i64(id)),
        envelope: EventEnvelope {
            kind,
            occurred_at,
            subject_type,
            subject_id: subject_id_i64.map(u64_from_i64),
            trace_id: trace_id.map(TraceId),
            payload: event,
        },
    })
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
        .map_err(|e| VoomError::Database(format!("rebuild Event for {kind:?}: {e}")))
}

#[expect(clippy::cast_possible_wrap, reason = "rowid fits i64")]
const fn i64_from_u64(v: u64) -> i64 {
    v as i64
}
#[expect(clippy::cast_sign_loss, reason = "rowid is non-negative")]
const fn u64_from_i64(v: i64) -> u64 {
    v as u64
}

#[cfg(test)]
#[path = "events_test.rs"]
mod tests;
