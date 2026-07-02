//! `voom job list|show` — durable job inspection with keyset pagination
//! (ADR 0031).

use std::io;

use serde::Serialize;
use voom_store::repo::jobs::{Job, JobFilter};

use crate::cli::{JobCommand, JobStateArg};
use crate::commands::common::{emit_voom_error, next_cursor, open_control_plane};
use crate::envelope::{Local, emit_err, emit_ok, emit_ok_page};

const COMMAND: &str = "job";

#[derive(Debug, Serialize)]
struct ListData {
    jobs: Vec<JobData>,
}

#[derive(Debug, Serialize)]
struct ShowData {
    job: JobData,
}

#[derive(Debug, Serialize)]
struct JobData {
    id: u64,
    kind: String,
    state: &'static str,
    priority: i64,
    created_at: String,
    updated_at: String,
    epoch: u64,
}

impl From<Job> for JobData {
    fn from(job: Job) -> Self {
        Self {
            id: job.id.0,
            kind: job.kind,
            state: job.state.as_str(),
            priority: job.priority,
            created_at: job.created_at.to_string(),
            updated_at: job.updated_at.to_string(),
            epoch: job.epoch,
        }
    }
}

pub async fn run(database_url: &str, local: Local, command: JobCommand) -> io::Result<i32> {
    match command {
        JobCommand::List {
            state,
            after_id,
            limit,
        } => list(database_url, local, state, after_id, limit).await,
        JobCommand::Show { job_id } => show(database_url, local, job_id).await,
    }
}

async fn list(
    database_url: &str,
    local: Local,
    state: Option<JobStateArg>,
    after_id: Option<u64>,
    limit: u32,
) -> io::Result<i32> {
    let cp = match open_control_plane(COMMAND, database_url, &local).await? {
        Ok(cp) => cp,
        Err(code) => return Ok(code),
    };
    let filter = JobFilter {
        state: state.map(JobStateArg::to_store),
    };
    match cp.list_jobs(filter, after_id, limit).await {
        Ok(jobs) => {
            let cursor = next_cursor(&jobs, limit, |job| job.id.0);
            emit_ok_page(
                COMMAND,
                ListData {
                    jobs: jobs.into_iter().map(JobData::from).collect(),
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

async fn show(database_url: &str, local: Local, job_id: u64) -> io::Result<i32> {
    let cp = match open_control_plane(COMMAND, database_url, &local).await? {
        Ok(cp) => cp,
        Err(code) => return Ok(code),
    };
    match cp.get_job(job_id).await {
        Ok(Some(job)) => emit_ok(
            COMMAND,
            ShowData {
                job: JobData::from(job),
            },
            Some(local),
            Vec::new(),
        )
        .map(|()| 0),
        Ok(None) => {
            emit_err(
                COMMAND,
                voom_core::ErrorCode::NotFound.as_str(),
                format!("job show: id={job_id} not found"),
                None,
                Some(local),
            )?;
            Ok(2)
        }
        Err(err) => emit_voom_error(COMMAND, &err, local),
    }
}
