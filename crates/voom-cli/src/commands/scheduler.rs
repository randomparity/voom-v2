use std::io;

use serde::Serialize;
use serde_json::Value as JsonValue;
use voom_core::{NodeId, TicketId, WorkerId};
use voom_store::repo::scheduler_decisions::{
    SchedulerDecision, SchedulerDecisionFilter, SchedulerDecisionOutcome,
};

use crate::cli::{SchedulerCommand, SchedulerDecisionCommand, SchedulerDecisionOutcomeArg};
use crate::commands::common::{emit_voom_error, open_control_plane};
use crate::envelope::{Local, emit_err, emit_ok};

#[derive(Debug, Serialize)]
struct ListData {
    decisions: Vec<DecisionSummaryData>,
}

#[derive(Debug, Serialize)]
struct ShowData {
    decision: DecisionData,
}

#[derive(Debug, Serialize)]
struct DecisionSummaryData {
    id: u64,
    created_at: String,
    outcome: &'static str,
    reason_code: String,
    summary: String,
    request_worker_id: Option<u64>,
    request_node_id: Option<u64>,
    ticket_id: Option<u64>,
    selected_worker_id: Option<u64>,
    selected_node_id: Option<u64>,
    selected_lease_id: Option<u64>,
    candidate_count: u32,
    selected_score: Option<i64>,
    suppressed_count: u32,
}

#[derive(Debug, Serialize)]
struct DecisionData {
    id: u64,
    created_at: String,
    updated_at: String,
    outcome: &'static str,
    reason_code: String,
    summary: String,
    request_worker_id: Option<u64>,
    request_node_id: Option<u64>,
    ticket_id: Option<u64>,
    selected_worker_id: Option<u64>,
    selected_node_id: Option<u64>,
    selected_lease_id: Option<u64>,
    candidate_count: u32,
    selected_score: Option<i64>,
    suppressed_count: u32,
    explanation_json: JsonValue,
}

pub async fn run(database_url: &str, local: Local, command: SchedulerCommand) -> io::Result<i32> {
    match command {
        SchedulerCommand::Decisions(SchedulerDecisionCommand::List {
            ticket_id,
            worker_id,
            node_id,
            outcome,
            limit,
        }) => {
            list(
                database_url,
                local,
                ticket_id,
                worker_id,
                node_id,
                outcome,
                limit,
            )
            .await
        }
        SchedulerCommand::Decisions(SchedulerDecisionCommand::Show { decision_id }) => {
            show(database_url, local, decision_id).await
        }
    }
}

async fn list(
    database_url: &str,
    local: Local,
    ticket_id: Option<u64>,
    worker_id: Option<u64>,
    node_id: Option<u64>,
    outcome: Option<SchedulerDecisionOutcomeArg>,
    limit: u32,
) -> io::Result<i32> {
    let cp = match open_control_plane("scheduler", database_url, &local).await? {
        Ok(cp) => cp,
        Err(code) => return Ok(code),
    };
    let filter = SchedulerDecisionFilter {
        ticket_id: ticket_id.map(TicketId),
        worker_id: worker_id.map(WorkerId),
        node_id: node_id.map(NodeId),
        outcome: outcome.map(outcome_arg_to_store),
        limit,
    };
    match cp.scheduler_decisions(filter).await {
        Ok(decisions) => emit_ok(
            "scheduler",
            ListData {
                decisions: decisions
                    .into_iter()
                    .map(DecisionSummaryData::from)
                    .collect(),
            },
            Some(local),
            Vec::new(),
        )
        .map(|()| 0),
        Err(err) => emit_voom_error("scheduler", &err, local),
    }
}

async fn show(database_url: &str, local: Local, decision_id: u64) -> io::Result<i32> {
    let cp = match open_control_plane("scheduler", database_url, &local).await? {
        Ok(cp) => cp,
        Err(code) => return Ok(code),
    };
    match cp.scheduler_decision(decision_id).await {
        Ok(Some(decision)) => emit_ok(
            "scheduler",
            ShowData {
                decision: DecisionData::from(decision),
            },
            Some(local),
            Vec::new(),
        )
        .map(|()| 0),
        Ok(None) => {
            emit_err(
                "scheduler",
                voom_core::ErrorCode::NotFound.as_str(),
                format!("scheduler decisions show: id={decision_id} not found"),
                None,
                Some(local),
            )?;
            Ok(2)
        }
        Err(err) => emit_voom_error("scheduler", &err, local),
    }
}

fn outcome_arg_to_store(outcome: SchedulerDecisionOutcomeArg) -> SchedulerDecisionOutcome {
    match outcome {
        SchedulerDecisionOutcomeArg::Selected => SchedulerDecisionOutcome::Selected,
        SchedulerDecisionOutcomeArg::Idle => SchedulerDecisionOutcome::Idle,
        SchedulerDecisionOutcomeArg::NoEligibleCandidate => {
            SchedulerDecisionOutcome::NoEligibleCandidate
        }
        SchedulerDecisionOutcomeArg::Rejected => SchedulerDecisionOutcome::Rejected,
    }
}

fn outcome_str(outcome: SchedulerDecisionOutcome) -> &'static str {
    outcome.as_str()
}

impl From<SchedulerDecision> for DecisionSummaryData {
    fn from(decision: SchedulerDecision) -> Self {
        Self {
            id: decision.id,
            created_at: decision.created_at.to_string(),
            outcome: outcome_str(decision.outcome),
            reason_code: decision.reason_code.as_str().to_owned(),
            summary: decision.summary,
            request_worker_id: decision.request_worker_id.map(|id| id.0),
            request_node_id: decision.request_node_id.map(|id| id.0),
            ticket_id: decision.ticket_id.map(|id| id.0),
            selected_worker_id: decision.selected_worker_id.map(|id| id.0),
            selected_node_id: decision.selected_node_id.map(|id| id.0),
            selected_lease_id: decision.selected_lease_id.map(|id| id.0),
            candidate_count: decision.candidate_count,
            selected_score: decision.selected_score,
            suppressed_count: decision.suppressed_count,
        }
    }
}

impl From<SchedulerDecision> for DecisionData {
    fn from(decision: SchedulerDecision) -> Self {
        Self {
            id: decision.id,
            created_at: decision.created_at.to_string(),
            updated_at: decision.updated_at.to_string(),
            outcome: outcome_str(decision.outcome),
            reason_code: decision.reason_code.as_str().to_owned(),
            summary: decision.summary,
            request_worker_id: decision.request_worker_id.map(|id| id.0),
            request_node_id: decision.request_node_id.map(|id| id.0),
            ticket_id: decision.ticket_id.map(|id| id.0),
            selected_worker_id: decision.selected_worker_id.map(|id| id.0),
            selected_node_id: decision.selected_node_id.map(|id| id.0),
            selected_lease_id: decision.selected_lease_id.map(|id| id.0),
            candidate_count: decision.candidate_count,
            selected_score: decision.selected_score,
            suppressed_count: decision.suppressed_count,
            explanation_json: decision.explanation,
        }
    }
}

#[cfg(test)]
#[path = "scheduler_test.rs"]
mod tests;
