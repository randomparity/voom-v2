//! `voom ticket list|show` — durable ticket inspection with keyset pagination
//! (ADR 0031).

use std::io;

use serde::Serialize;
use serde_json::Value as JsonValue;
use voom_store::repo::tickets::{Ticket, TicketFilter};

use crate::cli::{TicketCommand, TicketStateArg};
use crate::commands::common::{emit_voom_error, next_cursor, open_control_plane};
use crate::envelope::{Local, emit_err, emit_ok, emit_ok_page};

const COMMAND: &str = "ticket";

#[derive(Debug, Serialize)]
struct ListData {
    tickets: Vec<TicketSummaryData>,
}

#[derive(Debug, Serialize)]
struct ShowData {
    ticket: TicketData,
}

#[derive(Debug, Serialize)]
struct TicketSummaryData {
    id: u64,
    job_id: Option<u64>,
    kind: String,
    state: &'static str,
    priority: i64,
    attempt: u32,
    max_attempts: u32,
    next_eligible_at: String,
    created_at: String,
    state_changed_at: String,
    epoch: u64,
}

#[derive(Debug, Serialize)]
struct TicketData {
    id: u64,
    job_id: Option<u64>,
    kind: String,
    state: &'static str,
    priority: i64,
    attempt: u32,
    max_attempts: u32,
    next_eligible_at: String,
    created_at: String,
    state_changed_at: String,
    epoch: u64,
    payload: JsonValue,
    result: Option<JsonValue>,
}

impl From<Ticket> for TicketSummaryData {
    fn from(ticket: Ticket) -> Self {
        Self {
            id: ticket.id.0,
            job_id: ticket.job_id.map(|j| j.0),
            kind: ticket.kind.into_string(),
            state: ticket.state.as_str(),
            priority: ticket.priority,
            attempt: ticket.attempt,
            max_attempts: ticket.max_attempts,
            next_eligible_at: ticket.next_eligible_at.to_string(),
            created_at: ticket.created_at.to_string(),
            state_changed_at: ticket.state_changed_at.to_string(),
            epoch: ticket.epoch,
        }
    }
}

impl From<Ticket> for TicketData {
    fn from(ticket: Ticket) -> Self {
        Self {
            id: ticket.id.0,
            job_id: ticket.job_id.map(|j| j.0),
            kind: ticket.kind.into_string(),
            state: ticket.state.as_str(),
            priority: ticket.priority,
            attempt: ticket.attempt,
            max_attempts: ticket.max_attempts,
            next_eligible_at: ticket.next_eligible_at.to_string(),
            created_at: ticket.created_at.to_string(),
            state_changed_at: ticket.state_changed_at.to_string(),
            epoch: ticket.epoch,
            payload: ticket.payload,
            result: ticket.result,
        }
    }
}

pub async fn run(database_url: &str, local: Local, command: TicketCommand) -> io::Result<i32> {
    match command {
        TicketCommand::List {
            state,
            after_id,
            limit,
        } => list(database_url, local, state, after_id, limit).await,
        TicketCommand::Show { ticket_id } => show(database_url, local, ticket_id).await,
    }
}

async fn list(
    database_url: &str,
    local: Local,
    state: Option<TicketStateArg>,
    after_id: Option<u64>,
    limit: u32,
) -> io::Result<i32> {
    let cp = match open_control_plane(COMMAND, database_url, &local).await? {
        Ok(cp) => cp,
        Err(code) => return Ok(code),
    };
    let filter = TicketFilter {
        state: state.map(TicketStateArg::to_store),
    };
    match cp.list_tickets(filter, after_id, limit).await {
        Ok(tickets) => {
            let cursor = next_cursor(&tickets, limit, |ticket| ticket.id.0);
            emit_ok_page(
                COMMAND,
                ListData {
                    tickets: tickets.into_iter().map(TicketSummaryData::from).collect(),
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

async fn show(database_url: &str, local: Local, ticket_id: u64) -> io::Result<i32> {
    let cp = match open_control_plane(COMMAND, database_url, &local).await? {
        Ok(cp) => cp,
        Err(code) => return Ok(code),
    };
    match cp.get_ticket(ticket_id).await {
        Ok(Some(ticket)) => emit_ok(
            COMMAND,
            ShowData {
                ticket: TicketData::from(ticket),
            },
            Some(local),
            Vec::new(),
        )
        .map(|()| 0),
        Ok(None) => {
            emit_err(
                COMMAND,
                voom_core::ErrorCode::NotFound.as_str(),
                format!("ticket show: id={ticket_id} not found"),
                None,
                Some(local),
            )?;
            Ok(2)
        }
        Err(err) => emit_voom_error(COMMAND, &err, local),
    }
}
