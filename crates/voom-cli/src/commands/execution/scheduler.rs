use std::io;

use serde::Serialize;
use serde_json::Value as JsonValue;
use voom_core::{NodeId, TicketId, WorkerId};
use voom_store::repo::leases::{Lease, LeaseFilter};
use voom_store::repo::scheduler_decisions::{
    SchedulerDecision, SchedulerDecisionFilter, SchedulerDecisionOutcome,
};

use crate::cli::{
    LeaseStateArg, SchedulerCommand, SchedulerDecisionCommand, SchedulerDecisionOutcomeArg,
    SchedulerLeaseCommand,
};
use crate::commands::common::{emit_voom_error, next_cursor, open_control_plane};
use crate::envelope::{Local, emit_err, emit_ok, emit_ok_page};

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

#[derive(Debug)]
struct DecisionScalarData {
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

pub async fn run(database_url: &str, local: Local, command: SchedulerCommand) -> io::Result<i32> {
    match command {
        SchedulerCommand::Decisions(SchedulerDecisionCommand::List {
            ticket_id,
            worker_id,
            node_id,
            outcome,
            after_id,
            limit,
        }) => {
            list(
                database_url,
                local,
                DecisionListArgs {
                    ticket_id,
                    worker_id,
                    node_id,
                    outcome,
                    after_id,
                    limit,
                },
            )
            .await
        }
        SchedulerCommand::Decisions(SchedulerDecisionCommand::Show { decision_id }) => {
            show(database_url, local, decision_id).await
        }
        SchedulerCommand::Leases(SchedulerLeaseCommand::List {
            state,
            after_id,
            limit,
        }) => lease_list(database_url, local, state, after_id, limit).await,
        SchedulerCommand::Leases(SchedulerLeaseCommand::Show { lease_id }) => {
            lease_show(database_url, local, lease_id).await
        }
    }
}

struct DecisionListArgs {
    ticket_id: Option<u64>,
    worker_id: Option<u64>,
    node_id: Option<u64>,
    outcome: Option<SchedulerDecisionOutcomeArg>,
    after_id: Option<u64>,
    limit: u32,
}

async fn list(database_url: &str, local: Local, args: DecisionListArgs) -> io::Result<i32> {
    let cp = match open_control_plane("scheduler", database_url, &local).await? {
        Ok(cp) => cp,
        Err(code) => return Ok(code),
    };
    let filter = SchedulerDecisionFilter {
        ticket_id: args.ticket_id.map(TicketId),
        worker_id: args.worker_id.map(WorkerId),
        node_id: args.node_id.map(NodeId),
        outcome: args.outcome.map(outcome_arg_to_store),
        after_id: args.after_id,
        limit: args.limit,
    };
    match cp.scheduler_decisions(filter).await {
        Ok(decisions) => {
            let cursor = next_cursor(&decisions, args.limit, |decision| decision.id);
            emit_ok_page(
                "scheduler",
                ListData {
                    decisions: decisions
                        .into_iter()
                        .map(DecisionSummaryData::from)
                        .collect(),
                },
                cursor,
                Some(local),
                Vec::new(),
            )
            .map(|()| 0)
        }
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

#[derive(Debug, Serialize)]
struct LeaseListData {
    leases: Vec<LeaseData>,
}

#[derive(Debug, Serialize)]
struct LeaseShowData {
    lease: LeaseData,
}

#[derive(Debug, Serialize)]
struct LeaseData {
    id: u64,
    ticket_id: u64,
    worker_id: u64,
    state: &'static str,
    acquired_at: String,
    expires_at: String,
    last_heartbeat_at: String,
    ttl_seconds: i64,
    release_reason: Option<&'static str>,
    released_at: Option<String>,
    epoch: u64,
}

impl From<Lease> for LeaseData {
    fn from(lease: Lease) -> Self {
        Self {
            id: lease.id.0,
            ticket_id: lease.ticket_id.0,
            worker_id: lease.worker_id.0,
            state: lease.state.as_str(),
            acquired_at: lease.acquired_at.to_string(),
            expires_at: lease.expires_at.to_string(),
            last_heartbeat_at: lease.last_heartbeat_at.to_string(),
            ttl_seconds: lease.ttl_seconds,
            release_reason: lease
                .release_reason
                .map(voom_store::repo::leases::ReleaseReason::as_str),
            released_at: lease.released_at.map(|t| t.to_string()),
            epoch: lease.epoch,
        }
    }
}

async fn lease_list(
    database_url: &str,
    local: Local,
    state: Option<LeaseStateArg>,
    after_id: Option<u64>,
    limit: u32,
) -> io::Result<i32> {
    let cp = match open_control_plane("scheduler", database_url, &local).await? {
        Ok(cp) => cp,
        Err(code) => return Ok(code),
    };
    let filter = LeaseFilter {
        state: state.map(LeaseStateArg::to_store),
    };
    match cp.list_scheduler_leases(filter, after_id, limit).await {
        Ok(leases) => {
            let cursor = next_cursor(&leases, limit, |lease| lease.id.0);
            emit_ok_page(
                "scheduler",
                LeaseListData {
                    leases: leases.into_iter().map(LeaseData::from).collect(),
                },
                cursor,
                Some(local),
                Vec::new(),
            )
            .map(|()| 0)
        }
        Err(err) => emit_voom_error("scheduler", &err, local),
    }
}

async fn lease_show(database_url: &str, local: Local, lease_id: u64) -> io::Result<i32> {
    let cp = match open_control_plane("scheduler", database_url, &local).await? {
        Ok(cp) => cp,
        Err(code) => return Ok(code),
    };
    match cp.get_scheduler_lease(lease_id).await {
        Ok(Some(lease)) => emit_ok(
            "scheduler",
            LeaseShowData {
                lease: LeaseData::from(lease),
            },
            Some(local),
            Vec::new(),
        )
        .map(|()| 0),
        Ok(None) => {
            emit_err(
                "scheduler",
                voom_core::ErrorCode::NotFound.as_str(),
                format!("scheduler leases show: id={lease_id} not found"),
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

impl From<SchedulerDecision> for DecisionScalarData {
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

fn split_decision(mut decision: SchedulerDecision) -> (DecisionScalarData, String, JsonValue) {
    let updated_at = decision.updated_at.to_string();
    let explanation_json = std::mem::take(&mut decision.explanation);
    (
        DecisionScalarData::from(decision),
        updated_at,
        explanation_json,
    )
}

impl From<SchedulerDecision> for DecisionSummaryData {
    fn from(decision: SchedulerDecision) -> Self {
        let scalars = DecisionScalarData::from(decision);
        Self {
            id: scalars.id,
            created_at: scalars.created_at,
            outcome: scalars.outcome,
            reason_code: scalars.reason_code,
            summary: scalars.summary,
            request_worker_id: scalars.request_worker_id,
            request_node_id: scalars.request_node_id,
            ticket_id: scalars.ticket_id,
            selected_worker_id: scalars.selected_worker_id,
            selected_node_id: scalars.selected_node_id,
            selected_lease_id: scalars.selected_lease_id,
            candidate_count: scalars.candidate_count,
            selected_score: scalars.selected_score,
            suppressed_count: scalars.suppressed_count,
        }
    }
}

impl From<SchedulerDecision> for DecisionData {
    fn from(decision: SchedulerDecision) -> Self {
        let (scalars, updated_at, explanation_json) = split_decision(decision);
        Self {
            id: scalars.id,
            created_at: scalars.created_at,
            updated_at,
            outcome: scalars.outcome,
            reason_code: scalars.reason_code,
            summary: scalars.summary,
            request_worker_id: scalars.request_worker_id,
            request_node_id: scalars.request_node_id,
            ticket_id: scalars.ticket_id,
            selected_worker_id: scalars.selected_worker_id,
            selected_node_id: scalars.selected_node_id,
            selected_lease_id: scalars.selected_lease_id,
            candidate_count: scalars.candidate_count,
            selected_score: scalars.selected_score,
            suppressed_count: scalars.suppressed_count,
            explanation_json,
        }
    }
}

#[cfg(test)]
#[path = "scheduler_test.rs"]
mod tests;
