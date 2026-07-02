//! `voom event list|show` — durable event-journal inspection with
//! entity/kind/time filters and keyset pagination (ADR 0031).

use std::io;

use serde::Serialize;
use serde_json::Value as JsonValue;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use voom_events::{Event, EventKind, SubjectType};
use voom_store::repo::events::{EventFilter, EventRow};

use crate::cli::EventCommand;
use crate::commands::common::{emit_voom_error, next_cursor, open_control_plane};
use crate::envelope::{Local, emit_err, emit_ok, emit_ok_page};

const COMMAND: &str = "event";

#[derive(Debug, Serialize)]
struct ListData {
    events: Vec<EventData>,
}

#[derive(Debug, Serialize)]
struct ShowData {
    event: EventData,
}

#[derive(Debug, Serialize)]
struct EventData {
    id: u64,
    occurred_at: String,
    kind: String,
    subject_type: &'static str,
    subject_id: Option<u64>,
    trace_id: Option<String>,
    payload: JsonValue,
}

impl From<EventRow> for EventData {
    fn from(row: EventRow) -> Self {
        let kind = row.envelope.payload.kind().as_str().to_owned();
        Self {
            id: row.id.0,
            occurred_at: row.envelope.occurred_at.to_string(),
            kind,
            subject_type: row.envelope.subject_type.as_str(),
            subject_id: row.envelope.subject_id,
            trace_id: row.envelope.trace_id.map(|t| t.0),
            payload: inner_payload(&row.envelope.payload),
        }
    }
}

/// The event payload without its `kind` tag wrapper — `kind` is a sibling
/// field, so returning the tagged form here would duplicate it.
fn inner_payload(event: &Event) -> JsonValue {
    let value = serde_json::to_value(event).unwrap_or(JsonValue::Null);
    if let JsonValue::Object(map) = &value
        && let Some(payload) = map.get("payload")
    {
        return payload.clone();
    }
    value
}

pub async fn run(database_url: &str, local: Local, command: EventCommand) -> io::Result<i32> {
    match command {
        EventCommand::List {
            kind,
            subject_type,
            subject_id,
            since,
            until,
            after_id,
            limit,
        } => {
            list(
                database_url,
                local,
                ListArgs {
                    kind,
                    subject_type,
                    subject_id,
                    since,
                    until,
                    after_id,
                    limit,
                },
            )
            .await
        }
        EventCommand::Show { event_id } => show(database_url, local, event_id).await,
    }
}

struct ListArgs {
    kind: Option<String>,
    subject_type: Option<String>,
    subject_id: Option<u64>,
    since: Option<String>,
    until: Option<String>,
    after_id: Option<u64>,
    limit: u32,
}

async fn list(database_url: &str, local: Local, args: ListArgs) -> io::Result<i32> {
    let filter = match build_filter(&args) {
        Ok(filter) => filter,
        Err(message) => {
            emit_err(
                COMMAND,
                "BAD_ARGS",
                message,
                Some(
                    "kind/subject-type must be valid wire tokens; since/until must be RFC 3339"
                        .to_owned(),
                ),
                Some(local),
            )?;
            return Ok(1);
        }
    };
    let cp = match open_control_plane(COMMAND, database_url, &local).await? {
        Ok(cp) => cp,
        Err(code) => return Ok(code),
    };
    match cp.list_events(filter, args.after_id, args.limit).await {
        Ok(events) => {
            let cursor = next_cursor(&events, args.limit, |event| event.id.0);
            emit_ok_page(
                COMMAND,
                ListData {
                    events: events.into_iter().map(EventData::from).collect(),
                },
                cursor,
                Some(local),
                Vec::new(),
            )
            .map(|()| 0)
        }
        Err(err) => emit_voom_error(COMMAND, &err, local),
    }
}

fn build_filter(args: &ListArgs) -> Result<EventFilter, String> {
    let kind = args
        .kind
        .as_deref()
        .map(EventKind::from_str)
        .transpose()
        .map_err(|e| format!("invalid --kind: {e}"))?;
    let subject_type = args
        .subject_type
        .as_deref()
        .map(SubjectType::from_str)
        .transpose()
        .map_err(|e| format!("invalid --subject-type: {e}"))?;
    let since = parse_time(args.since.as_deref(), "--since")?;
    let until = parse_time(args.until.as_deref(), "--until")?;
    Ok(EventFilter {
        kind,
        subject_type,
        subject_id: args.subject_id,
        since,
        until,
    })
}

fn parse_time(value: Option<&str>, flag: &str) -> Result<Option<OffsetDateTime>, String> {
    value
        .map(|s| OffsetDateTime::parse(s, &Rfc3339))
        .transpose()
        .map_err(|e| format!("invalid {flag} timestamp {value:?}: {e}"))
}

async fn show(database_url: &str, local: Local, event_id: u64) -> io::Result<i32> {
    let cp = match open_control_plane(COMMAND, database_url, &local).await? {
        Ok(cp) => cp,
        Err(code) => return Ok(code),
    };
    match cp.get_event(event_id).await {
        Ok(Some(row)) => emit_ok(
            COMMAND,
            ShowData {
                event: EventData::from(row),
            },
            Some(local),
            Vec::new(),
        )
        .map(|()| 0),
        Ok(None) => {
            emit_err(
                COMMAND,
                voom_core::ErrorCode::NotFound.as_str(),
                format!("event show: id={event_id} not found"),
                None,
                Some(local),
            )?;
            Ok(2)
        }
        Err(err) => emit_voom_error(COMMAND, &err, local),
    }
}
