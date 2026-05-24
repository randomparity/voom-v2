# VOOM Sprint 9 Scheduler Scoring Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build Sprint 9 scheduler scoring: deterministic scored remote acquire, durable scheduler decision logs, node concurrency limits, and CLI decision inspection.

**Architecture:** Keep scoring policy in `voom-scheduler` as a pure, reusable Rust model. Keep persistence in `voom-store`, orchestration and transaction boundaries in `voom-control-plane`, HTTP response shape in `voom-api`, and JSON-envelope operator inspection in `voom-cli`.

**Tech Stack:** Rust, Tokio, sqlx/SQLite migrations, serde/serde_json, clap, axum, sibling unit tests, insta snapshots for CLI integration tests, `just` verification commands.

---

## File Structure

- Modify `crates/voom-scheduler/src/lib.rs`: replace the Sprint 2 single-worker-only public surface with a reusable scorer while preserving `WorkerSelector` compatibility for existing workflow code.
- Modify `crates/voom-scheduler/src/lib_test.rs`: pure scoring tests for gates, ordering, factor explanations, and tie-breakers.
- Create `migrations/0011_scheduler_decisions.sql`: scheduler node limits and durable scheduler decision logs.
- Create `crates/voom-store/src/repo/scheduler_decisions.rs`: repository, enums, row models, creation, suppression, selected-lease linking, get/list.
- Create `crates/voom-store/src/repo/scheduler_decisions_test.rs`: repository tests for selected, idle suppression, filtering, and selected-lease update.
- Modify `crates/voom-store/src/repo/mod.rs`: export the new repository.
- Modify `crates/voom-control-plane/src/lib.rs`: add `SqliteSchedulerDecisionRepo` to `ControlPlane`.
- Modify `crates/voom-control-plane/src/cases/remote_execution.rs`: route remote acquire through `voom-scheduler`, write decisions transactionally, return decision ids, enforce node limits.
- Modify `crates/voom-control-plane/src/cases/remote_execution_test.rs`: add scorer-backed selection, decision log, idempotent replay, no-candidate, and node concurrency coverage.
- Modify `crates/voom-api/src/execution.rs`: preserve remote acquire route while serializing new outcome fields.
- Modify `crates/voom-api/tests/remote_execution_route.rs`: assert decision ids in leased, idle, and replay responses.
- Modify `crates/voom-cli/src/cli.rs`: add `scheduler decisions list/show`.
- Modify `crates/voom-cli/src/main.rs`: dispatch scheduler CLI commands.
- Create `crates/voom-cli/src/commands/scheduler.rs`: emit scheduler decision JSON envelopes.
- Modify `crates/voom-cli/src/commands/mod.rs`: export scheduler command module.
- Create `crates/voom-cli/src/commands/scheduler_test.rs`: command-level tests for decision data mapping.
- Create `crates/voom-cli/tests/scheduler_envelope.rs`: integration tests and snapshots for list/show.
- Create snapshot files under `crates/voom-cli/tests/snapshots/` through `cargo insta review` after CLI output is intentionally accepted.
- Create `docs/superpowers/specs/2026-05-24-voom-sprint-9-closeout.md`: closeout matrix after implementation and verification.

## Task 1: Scheduler Scoring Core

**Files:**
- Modify: `crates/voom-scheduler/src/lib.rs`
- Modify: `crates/voom-scheduler/src/lib_test.rs`

- [ ] **Step 1: Write failing tests for hard gates and selected explanation**

Add these tests to `crates/voom-scheduler/src/lib_test.rs`:

```rust
use serde_json::json;
use voom_core::{NodeId, TicketId, WorkerId};

fn scored_candidate(
    ticket_id: u64,
    worker_id: u64,
    node_id: u64,
    operation: &str,
) -> SchedulerCandidate {
    SchedulerCandidate {
        ticket: TicketCandidate {
            ticket_id: TicketId(ticket_id),
            operation: operation.to_owned(),
            priority: 0,
            next_eligible_at_epoch_seconds: 0,
            payload: json!({
                "artifact_access": {
                    "inputs": ["handle:input:test"],
                    "outputs": ["handle:output:test"]
                }
            }),
        },
        worker: WorkerCandidate {
            worker_id: WorkerId(worker_id),
            node_id: NodeId(node_id),
            executable: true,
            has_capability: true,
            has_grant: true,
            denied: false,
            active_leases: 0,
            max_parallel: 2,
            artifact_access: vec!["shared_mount".to_owned()],
        },
        node: NodeCandidate {
            node_id: NodeId(node_id),
            executable: true,
            heartbeat_fresh: true,
            active_leases: 0,
            max_parallel_leases: 2,
        },
    }
}

#[test]
fn scorer_selects_eligible_candidate_with_explanation() {
    let scorer = SchedulerScorer::default();
    let out = scorer
        .score(&[scored_candidate(7, 11, 13, "probe_file")])
        .unwrap();

    assert_eq!(out.outcome, ScoreOutcome::Selected);
    assert_eq!(out.selected.as_ref().unwrap().ticket_id, TicketId(7));
    assert_eq!(out.selected.as_ref().unwrap().worker_id, WorkerId(11));
    assert_eq!(out.selected.as_ref().unwrap().node_id, NodeId(13));
    assert_eq!(out.selected.as_ref().unwrap().access_mode, "shared_mount");
    assert_eq!(out.explanation["scoring_version"], 1);
    assert_eq!(out.explanation["operation"], "probe_file");
    assert_eq!(out.explanation["candidates"][0]["eligible"], true);
    assert!(out.explanation["candidates"][0]["score"].as_i64().unwrap() > 0);
}

#[test]
fn scorer_rejects_hard_gate_failures_with_reason_codes() {
    let scorer = SchedulerScorer::default();
    let mut missing_grant = scored_candidate(1, 2, 3, "probe_file");
    missing_grant.worker.has_grant = false;
    let mut full_node = scored_candidate(4, 5, 6, "probe_file");
    full_node.node.active_leases = 2;
    full_node.node.max_parallel_leases = 2;

    let out = scorer.score(&[missing_grant, full_node]).unwrap();

    assert_eq!(out.outcome, ScoreOutcome::NoEligibleCandidate);
    let reasons0 = out.explanation["candidates"][0]["reasons"].as_array().unwrap();
    let reasons1 = out.explanation["candidates"][1]["reasons"].as_array().unwrap();
    assert!(reasons0.iter().any(|reason| reason == "missing_grant"));
    assert!(reasons1.iter().any(|reason| reason == "node_capacity_full"));
}
```

- [ ] **Step 2: Run scheduler tests to verify failure**

Run:

```bash
cargo test -p voom-scheduler scorer_selects_eligible_candidate_with_explanation
cargo test -p voom-scheduler scorer_rejects_hard_gate_failures_with_reason_codes
```

Expected: fails to compile because `SchedulerCandidate`, `TicketCandidate`, `WorkerCandidate`, `NodeCandidate`, `SchedulerScorer`, and `ScoreOutcome` do not exist.

- [ ] **Step 3: Implement minimal scoring model**

Add these public types and scorer to `crates/voom-scheduler/src/lib.rs` above the existing `WorkerSelector` trait:

```rust
use serde_json::{Value as JsonValue, json};
use voom_core::{NodeId, TicketId, VoomError, WorkerId};

pub const SCORING_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq)]
pub struct TicketCandidate {
    pub ticket_id: TicketId,
    pub operation: String,
    pub priority: i64,
    pub next_eligible_at_epoch_seconds: i64,
    pub payload: JsonValue,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerCandidate {
    pub worker_id: WorkerId,
    pub node_id: NodeId,
    pub executable: bool,
    pub has_capability: bool,
    pub has_grant: bool,
    pub denied: bool,
    pub active_leases: u32,
    pub max_parallel: u32,
    pub artifact_access: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeCandidate {
    pub node_id: NodeId,
    pub executable: bool,
    pub heartbeat_fresh: bool,
    pub active_leases: u32,
    pub max_parallel_leases: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SchedulerCandidate {
    pub ticket: TicketCandidate,
    pub worker: WorkerCandidate,
    pub node: NodeCandidate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScoreOutcome {
    Selected,
    Idle,
    NoEligibleCandidate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedCandidate {
    pub ticket_id: TicketId,
    pub worker_id: WorkerId,
    pub node_id: NodeId,
    pub access_mode: String,
    pub score: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ScoreDecision {
    pub outcome: ScoreOutcome,
    pub selected: Option<SelectedCandidate>,
    pub candidate_count: usize,
    pub reason_code: &'static str,
    pub explanation: JsonValue,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SchedulerScorer;

impl SchedulerScorer {
    pub fn score(&self, candidates: &[SchedulerCandidate]) -> Result<ScoreDecision, VoomError> {
        if candidates.is_empty() {
            return Ok(ScoreDecision {
                outcome: ScoreOutcome::Idle,
                selected: None,
                candidate_count: 0,
                reason_code: "no_ready_ticket",
                explanation: json!({
                    "scoring_version": SCORING_VERSION,
                    "operation": null,
                    "weights": weights_json(),
                    "candidates": []
                }),
            });
        }

        let operation = candidates[0].ticket.operation.clone();
        let mut rows = Vec::new();
        let mut best: Option<(SelectedCandidate, &SchedulerCandidate)> = None;

        for candidate in candidates {
            let reasons = hard_gate_reasons(candidate);
            let access_mode = select_access_mode(&candidate.worker.artifact_access);
            let eligible = reasons.is_empty() && access_mode.is_some();
            let score = if eligible { score_candidate(candidate, access_mode.unwrap()) } else { 0 };
            rows.push(json!({
                "ticket_id": candidate.ticket.ticket_id.0,
                "worker_id": candidate.worker.worker_id.0,
                "node_id": candidate.node.node_id.0,
                "eligible": eligible,
                "score": score,
                "selected_access_mode": access_mode,
                "factors": factor_json(candidate, access_mode, score),
                "reasons": reasons,
            }));
            if eligible {
                let selected = SelectedCandidate {
                    ticket_id: candidate.ticket.ticket_id,
                    worker_id: candidate.worker.worker_id,
                    node_id: candidate.node.node_id,
                    access_mode: access_mode.unwrap().to_owned(),
                    score,
                };
                if best.as_ref().is_none_or(|(current, current_candidate)| {
                    selected.score > current.score
                        || (selected.score == current.score
                            && tie_key(candidate) < tie_key(current_candidate))
                }) {
                    best = Some((selected, candidate));
                }
            }
        }

        if let Some((selected, _)) = best {
            Ok(ScoreDecision {
                outcome: ScoreOutcome::Selected,
                selected: Some(selected),
                candidate_count: candidates.len(),
                reason_code: "selected",
                explanation: json!({
                    "scoring_version": SCORING_VERSION,
                    "operation": operation,
                    "weights": weights_json(),
                    "candidates": rows
                }),
            })
        } else {
            Ok(ScoreDecision {
                outcome: ScoreOutcome::NoEligibleCandidate,
                selected: None,
                candidate_count: candidates.len(),
                reason_code: first_rejection_reason(&rows),
                explanation: json!({
                    "scoring_version": SCORING_VERSION,
                    "operation": operation,
                    "weights": weights_json(),
                    "candidates": rows
                }),
            })
        }
    }
}
```

Add helper functions in the same file:

```rust
fn weights_json() -> JsonValue {
    json!({
        "capability": 1000,
        "health": 500,
        "artifact_access": 100,
        "worker_capacity": 50,
        "node_capacity": 20,
        "locality": 20,
        "cost": 20,
        "tie_breaker": 1
    })
}

fn hard_gate_reasons(candidate: &SchedulerCandidate) -> Vec<&'static str> {
    let mut reasons = Vec::new();
    if !candidate.worker.has_capability {
        reasons.push("missing_capability");
    }
    if !candidate.worker.has_grant {
        reasons.push("missing_grant");
    }
    if candidate.worker.denied {
        reasons.push("operation_denied");
    }
    if !candidate.worker.executable {
        reasons.push("worker_not_executable");
    }
    if !candidate.node.executable {
        reasons.push("node_not_executable");
    }
    if !candidate.node.heartbeat_fresh {
        reasons.push("heartbeat_expired");
    }
    if select_access_mode(&candidate.worker.artifact_access).is_none() {
        reasons.push("unsupported_artifact_access");
    }
    if candidate.worker.active_leases >= candidate.worker.max_parallel {
        reasons.push("worker_capacity_full");
    }
    if candidate.node.active_leases >= candidate.node.max_parallel_leases {
        reasons.push("node_capacity_full");
    }
    reasons
}

fn select_access_mode(modes: &[String]) -> Option<&'static str> {
    if modes.iter().any(|mode| mode == "shared_mount") {
        Some("shared_mount")
    } else if modes.iter().any(|mode| mode == "control_plane_placeholder") {
        Some("control_plane_placeholder")
    } else if modes.iter().any(|mode| mode == "staged_output_placeholder") {
        Some("staged_output_placeholder")
    } else {
        None
    }
}

fn score_candidate(candidate: &SchedulerCandidate, access_mode: &str) -> i64 {
    let artifact_access = match access_mode {
        "shared_mount" => 100,
        "control_plane_placeholder" => 50,
        "staged_output_placeholder" => 25,
        _ => 0,
    };
    1000
        + 500
        + artifact_access
        + i64::from(candidate.worker.max_parallel.saturating_sub(candidate.worker.active_leases))
            * 50
        + i64::from(
            candidate
                .node
                .max_parallel_leases
                .saturating_sub(candidate.node.active_leases),
        ) * 20
}

fn factor_json(candidate: &SchedulerCandidate, access_mode: Option<&str>, score: i64) -> JsonValue {
    json!({
        "capability": if candidate.worker.has_capability && candidate.worker.has_grant && !candidate.worker.denied { 1000 } else { 0 },
        "health": if candidate.node.executable && candidate.node.heartbeat_fresh { 500 } else { 0 },
        "worker_capacity": i64::from(candidate.worker.max_parallel.saturating_sub(candidate.worker.active_leases)) * 50,
        "node_capacity": i64::from(candidate.node.max_parallel_leases.saturating_sub(candidate.node.active_leases)) * 20,
        "artifact_access": access_mode.map_or(0, |mode| match mode {
            "shared_mount" => 100,
            "control_plane_placeholder" => 50,
            "staged_output_placeholder" => 25,
            _ => 0,
        }),
        "tie_breaker": 0,
        "total": score
    })
}

fn tie_key(candidate: &SchedulerCandidate) -> (std::cmp::Reverse<i64>, i64, u64, u64, u64) {
    (
        std::cmp::Reverse(candidate.ticket.priority),
        candidate.ticket.next_eligible_at_epoch_seconds,
        candidate.node.node_id.0,
        candidate.worker.worker_id.0,
        candidate.ticket.ticket_id.0,
    )
}

fn first_rejection_reason(rows: &[JsonValue]) -> &'static str {
    rows.iter()
        .filter_map(|row| row["reasons"].as_array())
        .flat_map(|reasons| reasons.iter())
        .filter_map(serde_json::Value::as_str)
        .next()
        .unwrap_or("no_eligible_candidate")
}
```

Update `crates/voom-scheduler/Cargo.toml`:

```toml
[dependencies]
serde_json.workspace = true
voom-core.workspace = true
voom-worker-protocol.workspace = true
```

- [ ] **Step 4: Run tests**

Run:

```bash
cargo test -p voom-scheduler
```

Expected: all `voom-scheduler` tests pass. If clippy reports long-line or import ordering issues, run `just fmt` and rerun this command.

- [ ] **Step 5: Commit scheduler core**

Run:

```bash
git add crates/voom-scheduler/Cargo.toml crates/voom-scheduler/src/lib.rs crates/voom-scheduler/src/lib_test.rs
git commit -m "feat: add scheduler scoring core"
```

## Task 2: Scheduler Decision Schema And Repository

**Files:**
- Create: `migrations/0011_scheduler_decisions.sql`
- Create: `crates/voom-store/src/repo/scheduler_decisions.rs`
- Create: `crates/voom-store/src/repo/scheduler_decisions_test.rs`
- Modify: `crates/voom-store/src/repo/mod.rs`

- [ ] **Step 1: Write failing repository tests**

Create `crates/voom-store/src/repo/scheduler_decisions_test.rs`:

```rust
use serde_json::json;
use time::OffsetDateTime;
use voom_core::{LeaseId, NodeId, TicketId, WorkerId};

use super::*;

const T0: OffsetDateTime = OffsetDateTime::UNIX_EPOCH;

async fn repo() -> (SqliteSchedulerDecisionRepo, tempfile::NamedTempFile) {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let pool = crate::test_support::fresh_initialized_pool_at(tmp.path())
        .await
        .unwrap();
    seed_scheduler_refs(&pool).await;
    (SqliteSchedulerDecisionRepo::new(pool), tmp)
}

async fn seed_scheduler_refs(pool: &sqlx::SqlitePool) {
    sqlx::query(
        "INSERT INTO nodes \
         (id, name, kind, status, registered_at, last_seen_at, heartbeat_ttl_seconds, \
          auth_token_hash, auth_token_hint, metadata) \
         VALUES (3, 'node-3', 'remote', 'active', '1970-01-01T00:00:00Z', \
                 '1970-01-01T00:00:00Z', 60, 'hash', 'hint', '{}')",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO workers (id, name, kind, status, node_id, registered_at, last_seen_at) \
         VALUES (5, 'worker-5', 'remote', 'active', 3, \
                 '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z')",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO tickets \
         (id, job_id, kind, state, priority, payload, attempt, max_attempts, \
          next_eligible_at, created_at, state_changed_at) \
         VALUES (7, NULL, 'probe_file', 'leased', 0, '{}', 1, 3, \
                 '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z', \
                 '1970-01-01T00:00:00Z')",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO leases \
         (id, ticket_id, worker_id, state, acquired_at, expires_at, last_heartbeat_at, \
          ttl_seconds) \
         VALUES (11, 7, 5, 'held', '1970-01-01T00:00:00Z', \
                 '1970-01-01T00:01:00Z', '1970-01-01T00:00:00Z', 60)",
    )
    .execute(pool)
    .await
    .unwrap();
}

fn selected_input() -> NewSchedulerDecision {
    NewSchedulerDecision {
        decision_kind: SchedulerDecisionKind::LeaseAcquire,
        request_source: SchedulerRequestSource::RemoteAcquire,
        idempotency_key: Some("idem-1".to_owned()),
        request_node_id: Some(NodeId(3)),
        request_worker_id: Some(WorkerId(5)),
        ticket_id: Some(TicketId(7)),
        selected_worker_id: Some(WorkerId(5)),
        selected_node_id: Some(NodeId(3)),
        selected_lease_id: None,
        outcome: SchedulerDecisionOutcome::Selected,
        reason_code: SchedulerReasonCode::Selected,
        summary: "selected worker 5 for ticket 7".to_owned(),
        candidate_count: 1,
        selected_score: Some(1700),
        suppression_key: None,
        explanation: json!({"scoring_version":1,"candidates":[]}),
        now: T0,
    }
}

#[tokio::test]
async fn create_selected_and_link_lease_round_trip() {
    let (repo, _tmp) = repo().await;

    let created = repo.create(selected_input()).await.unwrap();
    assert_eq!(created.outcome, SchedulerDecisionOutcome::Selected);
    assert_eq!(created.selected_lease_id, None);

    let linked = repo
        .link_selected_lease(created.id, LeaseId(11), T0)
        .await
        .unwrap();

    assert_eq!(linked.selected_lease_id, Some(LeaseId(11)));
    assert_eq!(repo.get(created.id).await.unwrap().unwrap().selected_lease_id, Some(LeaseId(11)));
}

#[tokio::test]
async fn idle_decisions_are_suppressed_by_key() {
    let (repo, _tmp) = repo().await;
    let mut input = selected_input();
    input.decision_kind = SchedulerDecisionKind::Idle;
    input.outcome = SchedulerDecisionOutcome::Idle;
    input.reason_code = SchedulerReasonCode::NoReadyTicket;
    input.ticket_id = None;
    input.selected_worker_id = None;
    input.selected_node_id = None;
    input.selected_score = None;
    input.suppression_key = Some("remote_acquire:worker:5:no_ready_ticket:0".to_owned());

    let first = repo.create_or_suppress(input.clone()).await.unwrap();
    let second = repo.create_or_suppress(input).await.unwrap();

    assert_eq!(first.id, second.id);
    assert_eq!(second.suppressed_count, 1);
    assert_eq!(repo.list(SchedulerDecisionFilter::default()).await.unwrap().len(), 1);
}

#[tokio::test]
async fn list_filters_by_request_worker_and_outcome() {
    let (repo, _tmp) = repo().await;
    repo.create(selected_input()).await.unwrap();

    let rows = repo
        .list(SchedulerDecisionFilter {
            worker_id: Some(WorkerId(5)),
            outcome: Some(SchedulerDecisionOutcome::Selected),
            limit: 10,
            ..SchedulerDecisionFilter::default()
        })
        .await
        .unwrap();

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].request_worker_id, Some(WorkerId(5)));
}
```

- [ ] **Step 2: Run repository tests to verify failure**

Run:

```bash
cargo test -p voom-store scheduler_decisions
```

Expected: fails because `scheduler_decisions` repo module and migration do not exist.

- [ ] **Step 3: Add migration**

Create `migrations/0011_scheduler_decisions.sql`:

```sql
-- Sprint 9 - scheduler scoring, node limits, and durable decision logs.

CREATE TABLE scheduler_node_limits (
    node_id             INTEGER PRIMARY KEY REFERENCES nodes(id) ON DELETE CASCADE,
    max_parallel_leases INTEGER NOT NULL CHECK (max_parallel_leases > 0),
    created_at          TEXT NOT NULL,
    updated_at          TEXT NOT NULL
);

CREATE TABLE scheduler_decisions (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    created_at          TEXT NOT NULL,
    updated_at          TEXT NOT NULL,
    first_seen_at       TEXT NOT NULL,
    last_seen_at        TEXT NOT NULL,
    decision_kind       TEXT NOT NULL CHECK (decision_kind IN ('lease_acquire','idle','no_candidate')),
    request_source      TEXT NOT NULL CHECK (request_source IN ('remote_acquire')),
    idempotency_key     TEXT,
    request_node_id     INTEGER REFERENCES nodes(id) ON DELETE SET NULL,
    request_worker_id   INTEGER REFERENCES workers(id) ON DELETE SET NULL,
    ticket_id           INTEGER REFERENCES tickets(id) ON DELETE SET NULL,
    selected_worker_id  INTEGER REFERENCES workers(id) ON DELETE SET NULL,
    selected_node_id    INTEGER REFERENCES nodes(id) ON DELETE SET NULL,
    selected_lease_id   INTEGER REFERENCES leases(id) ON DELETE SET NULL,
    outcome             TEXT NOT NULL CHECK (outcome IN ('selected','idle','no_eligible_candidate','rejected')),
    reason_code         TEXT NOT NULL,
    summary             TEXT NOT NULL,
    candidate_count     INTEGER NOT NULL CHECK (candidate_count >= 0),
    selected_score      INTEGER,
    suppressed_count    INTEGER NOT NULL DEFAULT 0 CHECK (suppressed_count >= 0),
    suppression_key     TEXT,
    explanation_json    TEXT NOT NULL
);

CREATE INDEX scheduler_decisions_by_created_at
    ON scheduler_decisions (created_at DESC, id DESC);

CREATE INDEX scheduler_decisions_by_ticket
    ON scheduler_decisions (ticket_id, id);

CREATE INDEX scheduler_decisions_by_request_worker
    ON scheduler_decisions (request_worker_id, id);

CREATE INDEX scheduler_decisions_by_request_node
    ON scheduler_decisions (request_node_id, id);

CREATE INDEX scheduler_decisions_by_selected_worker
    ON scheduler_decisions (selected_worker_id, id);

CREATE INDEX scheduler_decisions_by_selected_node
    ON scheduler_decisions (selected_node_id, id);

CREATE INDEX scheduler_decisions_by_outcome
    ON scheduler_decisions (outcome, id);

CREATE INDEX scheduler_decisions_by_reason_code
    ON scheduler_decisions (reason_code, id);

CREATE UNIQUE INDEX scheduler_decisions_by_suppression_key
    ON scheduler_decisions (suppression_key)
    WHERE suppression_key IS NOT NULL;
```

- [ ] **Step 4: Implement repository module**

Create `crates/voom-store/src/repo/scheduler_decisions.rs` with enums and repo methods following `artifact_access_plans.rs` style:

```rust
//! Durable scheduler decision logs and scheduler-owned node limits.

use async_trait::async_trait;
use serde_json::Value as JsonValue;
use sqlx::{Row, SqlitePool};
use time::OffsetDateTime;
use voom_core::{LeaseId, NodeId, TicketId, VoomError, WorkerId};

use super::Repository;
use super::common::{
    i64_from_u64, iso8601, map_row_err, parse_iso8601, serialize_json, u64_from_i64,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SchedulerDecisionKind {
    LeaseAcquire,
    Idle,
    NoCandidate,
}

impl SchedulerDecisionKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LeaseAcquire => "lease_acquire",
            Self::Idle => "idle",
            Self::NoCandidate => "no_candidate",
        }
    }

    fn parse(s: &str) -> Result<Self, VoomError> {
        match s {
            "lease_acquire" => Ok(Self::LeaseAcquire),
            "idle" => Ok(Self::Idle),
            "no_candidate" => Ok(Self::NoCandidate),
            other => Err(VoomError::Database(format!(
                "scheduler_decisions.decision_kind {other:?} not in vocab"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SchedulerRequestSource {
    RemoteAcquire,
}

impl SchedulerRequestSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RemoteAcquire => "remote_acquire",
        }
    }

    fn parse(s: &str) -> Result<Self, VoomError> {
        match s {
            "remote_acquire" => Ok(Self::RemoteAcquire),
            other => Err(VoomError::Database(format!(
                "scheduler_decisions.request_source {other:?} not in vocab"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SchedulerDecisionOutcome {
    Selected,
    Idle,
    NoEligibleCandidate,
    Rejected,
}

impl SchedulerDecisionOutcome {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Selected => "selected",
            Self::Idle => "idle",
            Self::NoEligibleCandidate => "no_eligible_candidate",
            Self::Rejected => "rejected",
        }
    }

    fn parse(s: &str) -> Result<Self, VoomError> {
        match s {
            "selected" => Ok(Self::Selected),
            "idle" => Ok(Self::Idle),
            "no_eligible_candidate" => Ok(Self::NoEligibleCandidate),
            "rejected" => Ok(Self::Rejected),
            other => Err(VoomError::Database(format!(
                "scheduler_decisions.outcome {other:?} not in vocab"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SchedulerReasonCode {
    Selected,
    NoReadyTicket,
    MissingCapability,
    MissingGrant,
    OperationDenied,
    WorkerNotExecutable,
    NodeNotExecutable,
    HeartbeatExpired,
    UnsupportedArtifactAccess,
    WorkerCapacityFull,
    NodeCapacityFull,
    NoEligibleCandidate,
}

impl SchedulerReasonCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Selected => "selected",
            Self::NoReadyTicket => "no_ready_ticket",
            Self::MissingCapability => "missing_capability",
            Self::MissingGrant => "missing_grant",
            Self::OperationDenied => "operation_denied",
            Self::WorkerNotExecutable => "worker_not_executable",
            Self::NodeNotExecutable => "node_not_executable",
            Self::HeartbeatExpired => "heartbeat_expired",
            Self::UnsupportedArtifactAccess => "unsupported_artifact_access",
            Self::WorkerCapacityFull => "worker_capacity_full",
            Self::NodeCapacityFull => "node_capacity_full",
            Self::NoEligibleCandidate => "no_eligible_candidate",
        }
    }

    fn parse(s: &str) -> Result<Self, VoomError> {
        match s {
            "selected" => Ok(Self::Selected),
            "no_ready_ticket" => Ok(Self::NoReadyTicket),
            "missing_capability" => Ok(Self::MissingCapability),
            "missing_grant" => Ok(Self::MissingGrant),
            "operation_denied" => Ok(Self::OperationDenied),
            "worker_not_executable" => Ok(Self::WorkerNotExecutable),
            "node_not_executable" => Ok(Self::NodeNotExecutable),
            "heartbeat_expired" => Ok(Self::HeartbeatExpired),
            "unsupported_artifact_access" => Ok(Self::UnsupportedArtifactAccess),
            "worker_capacity_full" => Ok(Self::WorkerCapacityFull),
            "node_capacity_full" => Ok(Self::NodeCapacityFull),
            "no_eligible_candidate" => Ok(Self::NoEligibleCandidate),
            other => Err(VoomError::Database(format!(
                "scheduler_decisions.reason_code {other:?} not in vocab"
            ))),
        }
    }
}
```

Add these row and input structs after the enums:

```rust
#[derive(Debug, Clone)]
pub struct NewSchedulerDecision {
    pub decision_kind: SchedulerDecisionKind,
    pub request_source: SchedulerRequestSource,
    pub idempotency_key: Option<String>,
    pub request_node_id: Option<NodeId>,
    pub request_worker_id: Option<WorkerId>,
    pub ticket_id: Option<TicketId>,
    pub selected_worker_id: Option<WorkerId>,
    pub selected_node_id: Option<NodeId>,
    pub selected_lease_id: Option<LeaseId>,
    pub outcome: SchedulerDecisionOutcome,
    pub reason_code: SchedulerReasonCode,
    pub summary: String,
    pub candidate_count: u32,
    pub selected_score: Option<i64>,
    pub suppression_key: Option<String>,
    pub explanation: JsonValue,
    pub now: OffsetDateTime,
}

#[derive(Debug, Clone)]
pub struct SchedulerDecision {
    pub id: u64,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
    pub first_seen_at: OffsetDateTime,
    pub last_seen_at: OffsetDateTime,
    pub decision_kind: SchedulerDecisionKind,
    pub request_source: SchedulerRequestSource,
    pub idempotency_key: Option<String>,
    pub request_node_id: Option<NodeId>,
    pub request_worker_id: Option<WorkerId>,
    pub ticket_id: Option<TicketId>,
    pub selected_worker_id: Option<WorkerId>,
    pub selected_node_id: Option<NodeId>,
    pub selected_lease_id: Option<LeaseId>,
    pub outcome: SchedulerDecisionOutcome,
    pub reason_code: SchedulerReasonCode,
    pub summary: String,
    pub candidate_count: u32,
    pub selected_score: Option<i64>,
    pub suppressed_count: u32,
    pub suppression_key: Option<String>,
    pub explanation: JsonValue,
}

#[derive(Debug, Clone)]
pub struct SchedulerDecisionFilter {
    pub ticket_id: Option<TicketId>,
    pub worker_id: Option<WorkerId>,
    pub node_id: Option<NodeId>,
    pub outcome: Option<SchedulerDecisionOutcome>,
    pub limit: u32,
}

impl Default for SchedulerDecisionFilter {
    fn default() -> Self {
        Self {
            ticket_id: None,
            worker_id: None,
            node_id: None,
            outcome: None,
            limit: 100,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchedulerNodeLimit {
    pub node_id: NodeId,
    pub max_parallel_leases: u32,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}
```

Add trait methods:

```rust
#[derive(Debug, Clone)]
pub struct SqliteSchedulerDecisionRepo {
    pool: SqlitePool,
}

impl SqliteSchedulerDecisionRepo {
    #[must_use]
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

impl Repository for SqliteSchedulerDecisionRepo {}

#[async_trait]
pub trait SchedulerDecisionRepo: Repository {
    async fn create(&self, input: NewSchedulerDecision) -> Result<SchedulerDecision, VoomError>;
    async fn create_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        input: NewSchedulerDecision,
    ) -> Result<SchedulerDecision, VoomError>;
    async fn create_or_suppress(
        &self,
        input: NewSchedulerDecision,
    ) -> Result<SchedulerDecision, VoomError>;
    async fn create_or_suppress_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        input: NewSchedulerDecision,
    ) -> Result<SchedulerDecision, VoomError>;
    async fn link_selected_lease(
        &self,
        id: u64,
        lease_id: LeaseId,
        now: OffsetDateTime,
    ) -> Result<SchedulerDecision, VoomError>;
    async fn link_selected_lease_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        id: u64,
        lease_id: LeaseId,
        now: OffsetDateTime,
    ) -> Result<SchedulerDecision, VoomError>;
    async fn get(&self, id: u64) -> Result<Option<SchedulerDecision>, VoomError>;
    async fn list(
        &self,
        filter: SchedulerDecisionFilter,
    ) -> Result<Vec<SchedulerDecision>, VoomError>;
    async fn node_limit_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        node_id: NodeId,
    ) -> Result<u32, VoomError>;
    async fn set_node_limit(
        &self,
        node_id: NodeId,
        max_parallel_leases: u32,
        now: OffsetDateTime,
    ) -> Result<(), VoomError>;
}
```

At the bottom of `scheduler_decisions.rs`, wire the sibling unit test file:

```rust
#[cfg(test)]
#[path = "scheduler_decisions_test.rs"]
mod tests;
```

Implement `create_or_suppress_in_tx` with this SQL shape so rows with `suppression_key` update `suppressed_count`, `updated_at`, and `last_seen_at` instead of inserting duplicates:

```rust
let explanation = serialize_json(&input.explanation, "scheduler decision explanation")?;
let now = iso8601(input.now)?;
let decision = sqlx::query(
    "INSERT INTO scheduler_decisions \
     (created_at, updated_at, first_seen_at, last_seen_at, decision_kind, request_source, \
      idempotency_key, request_node_id, request_worker_id, ticket_id, selected_worker_id, \
      selected_node_id, selected_lease_id, outcome, reason_code, summary, candidate_count, \
      selected_score, suppressed_count, suppression_key, explanation_json) \
     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0, ?, ?) \
     ON CONFLICT(suppression_key) WHERE suppression_key IS NOT NULL DO UPDATE SET \
      updated_at = excluded.updated_at, \
      last_seen_at = excluded.last_seen_at, \
      suppressed_count = scheduler_decisions.suppressed_count + 1 \
     RETURNING id, created_at, updated_at, first_seen_at, last_seen_at, decision_kind, \
      request_source, idempotency_key, request_node_id, request_worker_id, ticket_id, \
      selected_worker_id, selected_node_id, selected_lease_id, outcome, reason_code, \
      summary, candidate_count, selected_score, suppressed_count, suppression_key, \
      explanation_json",
)
.bind(&now)
.bind(&now)
.bind(&now)
.bind(&now)
.bind(input.decision_kind.as_str())
.bind(input.request_source.as_str())
.bind(input.idempotency_key.as_deref())
.bind(input.request_node_id.map(|id| i64_from_u64(id.0)))
.bind(input.request_worker_id.map(|id| i64_from_u64(id.0)))
.bind(input.ticket_id.map(|id| i64_from_u64(id.0)))
.bind(input.selected_worker_id.map(|id| i64_from_u64(id.0)))
.bind(input.selected_node_id.map(|id| i64_from_u64(id.0)))
.bind(input.selected_lease_id.map(|id| i64_from_u64(id.0)))
.bind(input.outcome.as_str())
.bind(input.reason_code.as_str())
.bind(input.summary.as_str())
.bind(i64::from(input.candidate_count))
.bind(input.selected_score)
.bind(input.suppression_key.as_deref())
.bind(&explanation)
.fetch_one(&mut **tx)
.await
.map_err(|e| VoomError::Database(format!("scheduler_decisions insert/suppress: {e}")))?;
```

Decode the returned row with the same `SchedulerDecision` row mapper used by `get` and `list`. Do not use `last_insert_rowid()` for this path; after an upsert conflict it can still point at an unrelated prior insert on the same connection.

- [ ] **Step 5: Export repository**

Modify `crates/voom-store/src/repo/mod.rs`:

```rust
pub mod scheduler_decisions;

pub use scheduler_decisions::{
    NewSchedulerDecision, SchedulerDecision, SchedulerDecisionFilter, SchedulerDecisionKind,
    SchedulerDecisionOutcome, SchedulerDecisionRepo, SchedulerReasonCode, SchedulerRequestSource,
    SqliteSchedulerDecisionRepo,
};
```

- [ ] **Step 6: Run repository tests**

Run:

```bash
cargo test -p voom-store scheduler_decisions
```

Expected: scheduler decision repository tests pass.

- [ ] **Step 7: Commit persistence layer**

Run:

```bash
git add migrations/0011_scheduler_decisions.sql crates/voom-store/src/repo/mod.rs crates/voom-store/src/repo/scheduler_decisions.rs crates/voom-store/src/repo/scheduler_decisions_test.rs
git commit -m "feat: persist scheduler decisions"
```

## Task 3: ControlPlane Wiring And Remote Acquire Outcomes

**Files:**
- Modify: `crates/voom-control-plane/src/lib.rs`
- Modify: `crates/voom-control-plane/src/cases/remote_execution.rs`
- Modify: `crates/voom-control-plane/src/cases/remote_execution_test.rs`

- [ ] **Step 1: Write failing control-plane tests for decision ids and idle persistence**

Add to `crates/voom-control-plane/src/cases/remote_execution_test.rs`:

```rust
use voom_store::repo::scheduler_decisions::{
    SchedulerDecisionOutcome, SchedulerDecisionRepo,
};

#[tokio::test]
async fn remote_acquire_idle_returns_and_persists_scheduler_decision() {
    let fixture = remote_fixture(&[(OP, vec!["shared_mount"])], &[OP], &[]).await;

    let outcome = fixture
        .cp
        .remote_acquire(fixture.acquire_input("acquire-idle-decision", "hash-idle-decision"))
        .await
        .unwrap();

    let RemoteAcquireOutcome::Idle {
        worker_id,
        scheduler_decision_id,
    } = outcome
    else {
        panic!("expected idle outcome");
    };
    assert_eq!(worker_id, fixture.worker_id);
    let decision = fixture
        .cp
        .scheduler_decisions()
        .get(scheduler_decision_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(decision.outcome, SchedulerDecisionOutcome::Idle);
    assert_eq!(decision.request_worker_id, Some(fixture.worker_id));
}

#[tokio::test]
async fn remote_acquire_leased_returns_scheduler_decision_id_linked_to_lease() {
    let fixture = remote_fixture(&[(OP, vec!["shared_mount"])], &[OP], &[]).await;
    fixture.ready_ticket(OP).await;

    let outcome = fixture
        .cp
        .remote_acquire(fixture.acquire_input("leased-decision", "hash-leased-decision"))
        .await
        .unwrap();

    let RemoteAcquireOutcome::Leased(dispatch) = outcome else {
        panic!("expected leased outcome");
    };
    let decision = fixture
        .cp
        .scheduler_decisions()
        .get(dispatch.scheduler_decision_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(decision.selected_lease_id, Some(dispatch.lease_id));
    assert_eq!(decision.selected_worker_id, Some(fixture.worker_id));
}
```

- [ ] **Step 2: Run tests to verify failure**

Run:

```bash
cargo test -p voom-control-plane remote_acquire_idle_returns_and_persists_scheduler_decision
cargo test -p voom-control-plane remote_acquire_leased_returns_scheduler_decision_id_linked_to_lease
```

Expected: fails because `scheduler_decision_id` fields and `scheduler_decisions()` accessor do not exist.

- [ ] **Step 3: Add repository to ControlPlane**

Modify `crates/voom-control-plane/src/lib.rs` imports and struct:

```rust
use voom_store::repo::{
    scheduler_decisions::SqliteSchedulerDecisionRepo,
    /* existing imports */
};

pub(crate) scheduler_decisions: SqliteSchedulerDecisionRepo,
```

Initialize it in `new_unchecked`:

```rust
scheduler_decisions: SqliteSchedulerDecisionRepo::new(pool.clone()),
```

Add a public read-only inspection accessor near the other repo accessors. This is intentionally public because the CLI exposes scheduler decisions without duplicating store wiring:

```rust
#[must_use]
pub fn scheduler_decisions(&self) -> &SqliteSchedulerDecisionRepo {
    &self.scheduler_decisions
}
```

- [ ] **Step 4: Extend remote acquire outcome types**

Modify `RemoteAcquireOutcome` and `RemoteLeaseDispatch` in `remote_execution.rs`:

```rust
pub enum RemoteAcquireOutcome {
    Idle {
        worker_id: WorkerId,
        scheduler_decision_id: u64,
    },
    NoCandidate {
        worker_id: WorkerId,
        scheduler_decision_id: u64,
    },
    Leased(RemoteLeaseDispatch),
}

pub struct RemoteLeaseDispatch {
    pub lease_id: LeaseId,
    pub ticket_id: TicketId,
    pub worker_id: WorkerId,
    pub operation: String,
    pub dispatch_payload: JsonValue,
    pub lease_ttl_seconds: i64,
    pub heartbeat_after_seconds: i64,
    pub scheduler_decision_id: u64,
    pub artifact_access_plan: RemoteArtifactAccessPlan,
}
```

Update existing tests that pattern-match idle to include `scheduler_decision_id: _`.

- [ ] **Step 4a: Extend prepared acquire state**

Update the private `RemoteAcquirePrepared` enum in `remote_execution.rs` before changing preflight returns:

```rust
enum RemoteAcquirePrepared {
    Idle(RemoteAcquireOutcome),
    NoCandidate(RemoteAcquireOutcome),
    Leased {
        ticket: Ticket,
        eligibility: WorkerOperationEligibility,
        scheduler_decision: SchedulerDecision,
        selected_access_mode: ArtifactAccessMode,
    },
}
```

Update the `remote_acquire` match so both `Idle(outcome)` and `NoCandidate(outcome)` complete remote idempotency with the outcome and return success. The `Leased` arm must pass the carried `scheduler_decision` into `remote_acquire_leased_in_tx` so the created lease can be linked to the already-written decision row.

- [ ] **Step 5: Convert scorer output to durable decisions**

In `remote_execution.rs`, import scheduler and decision repo types:

```rust
use sqlx::Row;
use voom_scheduler::{
    NodeCandidate, SCORING_VERSION, SchedulerCandidate, SchedulerScorer, ScoreOutcome, TicketCandidate,
    WorkerCandidate,
};
use voom_store::repo::scheduler_decisions::{
    NewSchedulerDecision, SchedulerDecision, SchedulerDecisionKind, SchedulerDecisionOutcome,
    SchedulerReasonCode, SchedulerRequestSource,
};
```

Add mapping helpers:

```rust
fn decision_from_score(
    input: &RemoteAcquireInput,
    score: &voom_scheduler::ScoreDecision,
    selected: Option<&voom_scheduler::SelectedCandidate>,
    now: time::OffsetDateTime,
) -> NewSchedulerDecision {
    NewSchedulerDecision {
        decision_kind: match score.outcome {
            ScoreOutcome::Selected => SchedulerDecisionKind::LeaseAcquire,
            ScoreOutcome::Idle => SchedulerDecisionKind::Idle,
            ScoreOutcome::NoEligibleCandidate => SchedulerDecisionKind::NoCandidate,
        },
        request_source: SchedulerRequestSource::RemoteAcquire,
        idempotency_key: Some(input.idempotency_key.clone()),
        request_node_id: Some(input.node_id),
        request_worker_id: Some(input.worker_id),
        ticket_id: selected.map(|candidate| candidate.ticket_id),
        selected_worker_id: selected.map(|candidate| candidate.worker_id),
        selected_node_id: selected.map(|candidate| candidate.node_id),
        selected_lease_id: None,
        outcome: match score.outcome {
            ScoreOutcome::Selected => SchedulerDecisionOutcome::Selected,
            ScoreOutcome::Idle => SchedulerDecisionOutcome::Idle,
            ScoreOutcome::NoEligibleCandidate => SchedulerDecisionOutcome::NoEligibleCandidate,
        },
        reason_code: scheduler_reason(score.reason_code),
        summary: scheduler_summary(score),
        candidate_count: u32::try_from(score.candidate_count).unwrap_or(u32::MAX),
        selected_score: selected.map(|candidate| candidate.score),
        suppression_key: suppression_key(input, score),
        explanation: score.explanation.clone(),
        now,
    }
}
```

Add these mapping helpers:

```rust
fn scheduler_reason(reason: &str) -> SchedulerReasonCode {
    match reason {
        "selected" => SchedulerReasonCode::Selected,
        "no_ready_ticket" => SchedulerReasonCode::NoReadyTicket,
        "missing_capability" => SchedulerReasonCode::MissingCapability,
        "missing_grant" => SchedulerReasonCode::MissingGrant,
        "operation_denied" => SchedulerReasonCode::OperationDenied,
        "worker_not_executable" => SchedulerReasonCode::WorkerNotExecutable,
        "node_not_executable" => SchedulerReasonCode::NodeNotExecutable,
        "heartbeat_expired" => SchedulerReasonCode::HeartbeatExpired,
        "unsupported_artifact_access" => SchedulerReasonCode::UnsupportedArtifactAccess,
        "worker_capacity_full" => SchedulerReasonCode::WorkerCapacityFull,
        "node_capacity_full" => SchedulerReasonCode::NodeCapacityFull,
        _ => SchedulerReasonCode::NoEligibleCandidate,
    }
}

fn scheduler_summary(score: &voom_scheduler::ScoreDecision) -> String {
    match score.selected.as_ref() {
        Some(selected) => format!(
            "selected worker {} on node {} for ticket {}",
            selected.worker_id.0, selected.node_id.0, selected.ticket_id.0
        ),
        None => format!("scheduler outcome {}", score.reason_code),
    }
}

fn suppression_key(input: &RemoteAcquireInput, score: &voom_scheduler::ScoreDecision) -> Option<String> {
    if matches!(score.outcome, ScoreOutcome::Selected) {
        return None;
    }
    let bucket = input.lease_ttl_seconds.max(1) / 30;
    Some(format!(
        "remote_acquire:node:{}:worker:{}:reason:{}:bucket:{}",
        input.node_id.0, input.worker_id.0, score.reason_code, bucket
    ))
}
```

Still in Task 3, keep the existing first-eligible preflight loop, but create durable decisions on both return paths so this task can compile and pass before Task 4 replaces the loop with full scored candidate selection:

- For the empty-ticket path, call `SchedulerScorer::default().score(&[])?`, persist `decision_from_score(input, &score, None, now)` with `create_or_suppress_in_tx`, and return `RemoteAcquireOutcome::Idle { worker_id: input.worker_id, scheduler_decision_id: decision.id }`.
- For the current first eligible `Ok(())` path, create a selected decision with `create_in_tx`. Use `SchedulerDecisionKind::LeaseAcquire`, `SchedulerDecisionOutcome::Selected`, `SchedulerReasonCode::Selected`, `candidate_count: 1`, `selected_score: None`, `suppression_key: None`, and a minimal explanation such as `json!({"scoring_version": SCORING_VERSION, "interim": "first_eligible"})`. Carry that `SchedulerDecision` in `RemoteAcquirePrepared::Leased`.
- Change `remote_acquire_leased_in_tx` to accept the carried `SchedulerDecision`. After `acquire_lease_in_tx` succeeds and before building the response, call `link_selected_lease_in_tx(tx, scheduler_decision.id, lease.id, now)` and set `RemoteLeaseDispatch.scheduler_decision_id` from the linked decision id.

Task 4 removes the interim selected-decision explanation by replacing first-eligible selection with scored candidate construction. Do not leave the `"interim"` explanation after Task 4.

- [ ] **Step 6: Run control-plane tests**

Run:

```bash
cargo test -p voom-control-plane remote_acquire
```

Expected: all remote acquire tests pass after updating changed pattern matches.

- [ ] **Step 7: Commit control-plane outcome plumbing**

Run:

```bash
git add crates/voom-control-plane/src/lib.rs crates/voom-control-plane/src/cases/remote_execution.rs crates/voom-control-plane/src/cases/remote_execution_test.rs
git commit -m "feat: record remote scheduler decisions"
```

## Task 4: Scored Remote Selection And Node Concurrency

**Files:**
- Modify: `crates/voom-control-plane/src/cases/remote_execution.rs`
- Modify: `crates/voom-control-plane/src/cases/remote_execution_test.rs`
- Modify: `crates/voom-store/src/repo/scheduler_decisions.rs`

- [ ] **Step 1: Write failing tests for scored ordering and no-candidate**

Add to `remote_execution_test.rs`:

```rust
#[tokio::test]
async fn remote_acquire_uses_scored_priority_then_tie_breaker() {
    let fixture = remote_fixture(&[(OP, vec!["shared_mount"])], &[OP], &[]).await;
    let low = fixture.ready_ticket_with_priority(OP, 0).await;
    let high = fixture.ready_ticket_with_priority(OP, 10).await;

    let outcome = fixture
        .cp
        .remote_acquire(fixture.acquire_input("scored-priority", "hash-scored-priority"))
        .await
        .unwrap();

    let RemoteAcquireOutcome::Leased(dispatch) = outcome else {
        panic!("expected leased outcome");
    };
    assert_eq!(dispatch.ticket_id, high);
    assert_ne!(dispatch.ticket_id, low);
}

#[tokio::test]
async fn remote_acquire_no_candidate_is_success_with_decision() {
    let fixture = remote_fixture(&[(OP, vec!["local_path"])], &[OP], &[]).await;
    fixture.ready_ticket(OP).await;

    let outcome = fixture
        .cp
        .remote_acquire(fixture.acquire_input("no-candidate", "hash-no-candidate"))
        .await
        .unwrap();

    let RemoteAcquireOutcome::NoCandidate {
        worker_id,
        scheduler_decision_id,
    } = outcome
    else {
        panic!("expected no-candidate outcome");
    };
    assert_eq!(worker_id, fixture.worker_id);
    let decision = fixture
        .cp
        .scheduler_decisions()
        .get(scheduler_decision_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(decision.reason_code.as_str(), "unsupported_artifact_access");
}
```

- [ ] **Step 2: Write failing node concurrency test**

Add to `remote_execution_test.rs`:

```rust
#[tokio::test]
async fn node_default_limit_blocks_second_concurrent_remote_acquire() {
    let fixture = remote_fixture(&[(OP, vec!["shared_mount"])], &[OP], &[]).await;
    fixture.ready_ticket_with_priority(OP, 10).await;
    fixture.ready_ticket_with_priority(OP, 9).await;

    let first = fixture
        .cp
        .remote_acquire(fixture.acquire_input("node-limit-1", "hash-node-limit-1"))
        .await
        .unwrap();
    let RemoteAcquireOutcome::Leased(_) = first else {
        panic!("first acquire should lease");
    };

    let second = fixture
        .cp
        .remote_acquire(fixture.acquire_input("node-limit-2", "hash-node-limit-2"))
        .await
        .unwrap();

    let RemoteAcquireOutcome::NoCandidate {
        scheduler_decision_id,
        ..
    } = second
    else {
        panic!("second acquire should be no-candidate under node default limit");
    };
    let decision = fixture
        .cp
        .scheduler_decisions()
        .get(scheduler_decision_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(decision.reason_code.as_str(), "node_capacity_full");
}
```

- [ ] **Step 3: Run tests to verify failure**

Run:

```bash
cargo test -p voom-control-plane remote_acquire_uses_scored_priority_then_tie_breaker
cargo test -p voom-control-plane remote_acquire_no_candidate_is_success_with_decision
cargo test -p voom-control-plane node_default_limit_blocks_second_concurrent_remote_acquire
```

Expected: at least the no-candidate and node-limit tests fail until remote acquire uses scorer candidates and node active lease counts.

- [ ] **Step 4: Build scorer candidates inside remote acquire**

In `remote_acquire_preflight_in_tx`, replace the first-eligible loop with candidate construction:

```rust
let operations = worker_candidate_operations_in_tx(tx, input.worker_id).await?;
let tickets = self.tickets.ready_for_operations_in_tx(tx, &operations, now).await?;
if tickets.is_empty() {
    let score = SchedulerScorer::default().score(&[])?;
    let decision = self
        .scheduler_decisions
        .create_or_suppress_in_tx(tx, decision_from_score(input, &score, None, now))
        .await?;
    return Ok(RemoteAcquirePrepared::Idle(RemoteAcquireOutcome::Idle {
        worker_id: input.worker_id,
        scheduler_decision_id: decision.id,
    }));
}

let node_limit = self.scheduler_decisions.node_limit_in_tx(tx, input.node_id).await?;
let node_active_leases = active_lease_count_for_node_in_tx(tx, input.node_id).await?;
let mut prepared_candidates = Vec::new();
for ticket in tickets {
    let eligibility = self
        .workers
        .operation_eligibility_in_tx(tx, input.worker_id, &ticket.kind)
        .await?;
    let worker_active = active_lease_count_for_worker_operation_in_tx(tx, input.worker_id, &ticket.kind).await?;
    let worker_limit = max_parallel_for_worker_operation_in_tx(tx, input.worker_id, &ticket.kind).await?;
    let candidate = candidate_from_ticket(
        input,
        &ticket,
        &eligibility,
        worker_active,
        worker_limit,
        node_active_leases,
        node_limit,
    )?;
    prepared_candidates.push((ticket, eligibility, candidate));
}
let candidates: Vec<_> = prepared_candidates
    .iter()
    .map(|(_, _, candidate)| candidate.clone())
    .collect();
let score = SchedulerScorer::default().score(&candidates)?;
```

Add candidate construction and SQL helper functions near existing helpers:

```rust
fn candidate_from_ticket(
    input: &RemoteAcquireInput,
    ticket: &Ticket,
    eligibility: &WorkerOperationEligibility,
    worker_active: u32,
    worker_limit: u32,
    node_active: u32,
    node_limit: u32,
) -> Result<SchedulerCandidate, VoomError> {
    Ok(SchedulerCandidate {
        ticket: TicketCandidate {
            ticket_id: ticket.id,
            operation: ticket.kind.clone(),
            priority: ticket.priority,
            next_eligible_at_epoch_seconds: ticket.next_eligible_at.unix_timestamp(),
            payload: ticket.payload.clone(),
        },
        worker: WorkerCandidate {
            worker_id: input.worker_id,
            node_id: input.node_id,
            executable: true,
            has_capability: eligibility.has_capability,
            has_grant: eligibility.has_grant,
            denied: eligibility.is_denied,
            active_leases: worker_active,
            max_parallel: worker_limit,
            artifact_access: eligibility.artifact_access.clone(),
        },
        node: NodeCandidate {
            node_id: input.node_id,
            executable: true,
            heartbeat_fresh: true,
            active_leases: node_active,
            max_parallel_leases: node_limit,
        },
    })
}
```

Keep node `executable` and `heartbeat_fresh` true here because `validate_remote_node_live` already rejected inactive, retired, or stale nodes before preflight. If that validation moves later, this helper must consume the checked node state instead of hardcoding true.

Add the node active count helper:

```rust
async fn active_lease_count_for_node_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    node_id: NodeId,
) -> Result<u32, VoomError> {
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) \
         FROM leases l \
         JOIN workers w ON w.id = l.worker_id \
         WHERE w.node_id = ? AND l.state = 'held'",
    )
    .bind(i64::try_from(node_id.0).map_err(|_| {
        VoomError::Config(format!("node id {} does not fit sqlite i64", node_id.0))
    })?)
    .fetch_one(&mut **tx)
    .await
    .map_err(|e| VoomError::Database(format!("scheduler node active leases: {e}")))?;
    u32::try_from(count)
        .map_err(|_| VoomError::Database(format!("node active lease count {count} invalid")))
}
```

Add the worker operation count helper:

```rust
async fn active_lease_count_for_worker_operation_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    worker_id: WorkerId,
    operation: &str,
) -> Result<u32, VoomError> {
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) \
         FROM leases l \
         JOIN tickets t ON t.id = l.ticket_id \
         WHERE l.worker_id = ? AND l.state = 'held' AND t.kind = ?",
    )
    .bind(i64::try_from(worker_id.0).map_err(|_| {
        VoomError::Config(format!("worker id {} does not fit sqlite i64", worker_id.0))
    })?)
    .bind(operation)
    .fetch_one(&mut **tx)
    .await
    .map_err(|e| VoomError::Database(format!("scheduler worker active leases: {e}")))?;
    u32::try_from(count)
        .map_err(|_| VoomError::Database(format!("worker active lease count {count} invalid")))
}
```

Add a worker max-parallel helper by reading `worker_grants.max_parallel`, matching the existing executor semantics of `operation`, then `"*"`, then default `1`:

```rust
async fn max_parallel_for_worker_operation_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    worker_id: WorkerId,
    operation: &str,
) -> Result<u32, VoomError> {
    let rows = sqlx::query("SELECT max_parallel FROM worker_grants WHERE worker_id = ? ORDER BY id ASC")
        .bind(i64::try_from(worker_id.0).map_err(|_| {
            VoomError::Config(format!("worker id {} does not fit sqlite i64", worker_id.0))
        })?)
        .fetch_all(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("scheduler worker max_parallel: {e}")))?;
    let mut max_parallel = 1;
    for row in rows {
        let raw: String = row
            .try_get("max_parallel")
            .map_err(|e| VoomError::Database(format!("scheduler worker max_parallel row: {e}")))?;
        let value: serde_json::Value = serde_json::from_str(&raw)
            .map_err(|e| VoomError::Database(format!("parse scheduler worker max_parallel: {e}")))?;
        let grant_max = value
            .get(operation)
            .or_else(|| value.get("*"))
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(1);
        max_parallel = max_parallel.max(u32::try_from(grant_max).unwrap_or(u32::MAX).max(1));
    }
    Ok(max_parallel)
}
```

- [ ] **Step 5: Return selected/no-candidate outcomes**

When `score.outcome == ScoreOutcome::NoEligibleCandidate`, create or suppress the decision and return:

```rust
let decision = self
    .scheduler_decisions
    .create_or_suppress_in_tx(tx, decision_from_score(input, &score, None, now))
    .await?;
RemoteAcquirePrepared::NoCandidate(RemoteAcquireOutcome::NoCandidate {
    worker_id: input.worker_id,
    scheduler_decision_id: decision.id,
})
```

When selected, identify the selected tuple, run the post-score capacity recheck from Step 6, then persist the selected decision immediately before returning the leased preparation:

```rust
let selected = score
    .selected
    .as_ref()
    .ok_or_else(|| VoomError::Internal("selected scheduler outcome missing candidate".to_owned()))?;
let (ticket, eligibility, _) = prepared_candidates
    .iter()
    .find(|(ticket, _, _)| ticket.id == selected.ticket_id)
    .ok_or_else(|| VoomError::Internal("selected scheduler ticket missing from candidates".to_owned()))?;
// Step 6 capacity recheck happens here.
let decision = self
    .scheduler_decisions
    .create_in_tx(tx, decision_from_score(input, &score, Some(selected), now))
    .await?;
```

Clone the recovered `ticket` and `eligibility` into the prepared leased outcome. Convert `selected.access_mode` back to `ArtifactAccessMode` with a small match over `shared_mount`, `control_plane_placeholder`, and `staged_output_placeholder`; unknown modes should return `VoomError::Internal`. Do not rescore after creating the decision; `remote_acquire_leased_in_tx` links the created lease to this exact decision row.

- [ ] **Step 6: Enforce capacity after scoring**

Before `acquire_lease_in_tx`, re-read worker and node active counts for the selected tuple. If either limit is full, write a no-candidate decision with `worker_capacity_full` or `node_capacity_full` and return no-candidate. Use the same transaction.

For this post-score race check, do not call `decision_from_score` on the earlier selected score because that would persist `outcome = selected`. Instead build a fresh `NewSchedulerDecision` with `decision_kind: SchedulerDecisionKind::NoCandidate`, `outcome: SchedulerDecisionOutcome::NoEligibleCandidate`, the capacity reason code, `ticket_id` and selected ids set to `None`, `selected_score: None`, `candidate_count: 1`, a suppression key containing the capacity reason, and an explanation that includes the selected ticket id plus the observed active count and limit.

- [ ] **Step 7: Run control-plane remote tests**

Run:

```bash
cargo test -p voom-control-plane remote
```

Expected: all remote execution tests pass.

- [ ] **Step 8: Commit scored remote acquire**

Run:

```bash
git add crates/voom-control-plane/src/cases/remote_execution.rs crates/voom-control-plane/src/cases/remote_execution_test.rs crates/voom-store/src/repo/scheduler_decisions.rs
git commit -m "feat: score remote lease acquisition"
```

## Task 5: Remote API Response Contract

**Files:**
- Modify: `crates/voom-api/tests/remote_execution_route.rs`
- Modify: `crates/voom-api/src/execution.rs` if the compiler requires route-local type imports after control-plane outcome changes.

- [ ] **Step 1: Update API tests for scheduler decision ids**

Modify `acquire_returns_idle_as_success` in `remote_execution_route.rs`:

```rust
assert_eq!(json["data"]["outcome"], "idle");
assert!(json["data"]["scheduler_decision_id"].as_u64().unwrap() > 0);
```

Modify `ApiFixture::acquire_lease`:

```rust
assert!(json["data"]["scheduler_decision_id"].as_u64().unwrap() > 0);
```

Modify `acquire_same_key_replays_and_different_body_conflicts` to keep the existing full JSON equality assertion; this proves decision id replay.

- [ ] **Step 2: Run API tests**

Run:

```bash
cargo test -p voom-api remote_execution_route
```

Expected: pass after control-plane outcome structs serialize the new field.

- [ ] **Step 3: Commit API contract**

Run:

```bash
git add crates/voom-api/src/execution.rs crates/voom-api/tests/remote_execution_route.rs
git commit -m "test: assert scheduler decision ids in remote API"
```

## Task 6: CLI Scheduler Decision Inspection

**Files:**
- Modify: `crates/voom-cli/src/cli.rs`
- Modify: `crates/voom-cli/src/main.rs`
- Modify: `crates/voom-cli/src/commands/mod.rs`
- Create: `crates/voom-cli/src/commands/scheduler.rs`
- Create: `crates/voom-cli/src/commands/scheduler_test.rs`

- [ ] **Step 1: Add CLI argument model**

Modify `crates/voom-cli/src/cli.rs`:

```rust
#[derive(Subcommand, Debug, Clone)]
pub enum SchedulerCommand {
    /// Inspect scheduler decisions.
    #[command(subcommand)]
    Decisions(SchedulerDecisionCommand),
}

#[derive(Subcommand, Debug, Clone)]
pub enum SchedulerDecisionCommand {
    /// List scheduler decisions.
    List {
        #[arg(long)]
        ticket_id: Option<u64>,
        #[arg(long)]
        worker_id: Option<u64>,
        #[arg(long)]
        node_id: Option<u64>,
        #[arg(long)]
        outcome: Option<SchedulerDecisionOutcomeArg>,
        #[arg(long, default_value_t = 100)]
        limit: u32,
    },
    /// Show one scheduler decision.
    Show {
        #[arg(long)]
        decision_id: u64,
    },
}

#[derive(Copy, Clone, Debug, ValueEnum, PartialEq, Eq)]
#[value(rename_all = "snake_case")]
pub enum SchedulerDecisionOutcomeArg {
    Selected,
    Idle,
    NoEligibleCandidate,
    Rejected,
}
```

Add `Scheduler(SchedulerCommand)` to `Command`.

- [ ] **Step 2: Create command module tests**

Create `crates/voom-cli/src/commands/scheduler_test.rs`:

```rust
use serde_json::json;
use time::OffsetDateTime;
use voom_core::{NodeId, TicketId, WorkerId};
use voom_store::repo::scheduler_decisions::{
    NewSchedulerDecision, SchedulerDecisionKind, SchedulerDecisionOutcome, SchedulerDecisionRepo,
    SchedulerReasonCode, SchedulerRequestSource,
};

use super::*;

#[tokio::test]
async fn decision_data_maps_full_record() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let url = voom_store::test_support::sqlite_url_for(tmp.path());
    voom_store::init(&url).await.unwrap();
    let pool = voom_store::connect(&url).await.unwrap();
    seed_refs(&pool).await;
    let cp = voom_control_plane::ControlPlane::open(&url).await.unwrap();
    let created = cp
        .scheduler_decisions()
        .create(NewSchedulerDecision {
            decision_kind: SchedulerDecisionKind::LeaseAcquire,
            request_source: SchedulerRequestSource::RemoteAcquire,
            idempotency_key: Some("idem".to_owned()),
            request_node_id: Some(NodeId(1)),
            request_worker_id: Some(WorkerId(2)),
            ticket_id: Some(TicketId(3)),
            selected_worker_id: Some(WorkerId(2)),
            selected_node_id: Some(NodeId(1)),
            selected_lease_id: None,
            outcome: SchedulerDecisionOutcome::Selected,
            reason_code: SchedulerReasonCode::Selected,
            summary: "selected".to_owned(),
            candidate_count: 1,
            selected_score: Some(100),
            suppression_key: None,
            explanation: json!({"scoring_version":1}),
            now: OffsetDateTime::UNIX_EPOCH,
        })
        .await
        .unwrap();

    let data = DecisionData::from(created);

    assert_eq!(data.id, 1);
    assert_eq!(data.outcome, "selected");
    assert_eq!(data.explanation_json, json!({"scoring_version":1}));
}

async fn seed_refs(pool: &sqlx::SqlitePool) {
    sqlx::query(
        "INSERT INTO nodes \
         (id, name, kind, status, registered_at, last_seen_at, heartbeat_ttl_seconds, \
          auth_token_hash, auth_token_hint, metadata) \
         VALUES (1, 'node-1', 'remote', 'active', '1970-01-01T00:00:00Z', \
                 '1970-01-01T00:00:00Z', 60, 'hash', 'hint', '{}')",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO workers (id, name, kind, status, node_id, registered_at, last_seen_at) \
         VALUES (2, 'worker-2', 'remote', 'active', 1, \
                 '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z')",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO tickets \
         (id, job_id, kind, state, priority, payload, attempt, max_attempts, \
          next_eligible_at, created_at, state_changed_at) \
         VALUES (3, NULL, 'probe_file', 'ready', 0, '{}', 0, 3, \
                 '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z', \
                 '1970-01-01T00:00:00Z')",
    )
    .execute(pool)
    .await
    .unwrap();
}
```

- [ ] **Step 3: Implement scheduler command module**

Create `crates/voom-cli/src/commands/scheduler.rs`:

```rust
use std::io;

use serde::Serialize;
use serde_json::Value as JsonValue;
use voom_core::{NodeId, TicketId, WorkerId};
use voom_store::repo::scheduler_decisions::{
    SchedulerDecision, SchedulerDecisionFilter, SchedulerDecisionOutcome,
};

use crate::cli::{SchedulerCommand, SchedulerDecisionCommand, SchedulerDecisionOutcomeArg};
use crate::commands::common::{emit_voom_error, open_control_plane};
use crate::envelope::{Local, emit_ok};

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
```

Implement `run(database_url, local, command)` with this structure:

```rust
pub async fn run(
    database_url: &str,
    local: Local,
    command: SchedulerCommand,
) -> io::Result<i32> {
    match command {
        SchedulerCommand::Decisions(SchedulerDecisionCommand::List {
            ticket_id,
            worker_id,
            node_id,
            outcome,
            limit,
        }) => list(database_url, local, ticket_id, worker_id, node_id, outcome, limit).await,
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
    match cp.scheduler_decisions().list(filter).await {
        Ok(decisions) => emit_ok(
            "scheduler",
            ListData {
                decisions: decisions.into_iter().map(DecisionSummaryData::from).collect(),
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
    match cp.scheduler_decisions().get(decision_id).await {
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
            crate::envelope::emit_err(
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
```

Add the conversion helpers used above:

```rust
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
```

At the bottom of `scheduler.rs`, wire the sibling unit test file:

```rust
#[cfg(test)]
#[path = "scheduler_test.rs"]
mod tests;
```

- [ ] **Step 4: Wire CLI dispatch**

Modify `crates/voom-cli/src/commands/mod.rs`:

```rust
pub mod scheduler;
```

Modify imports and dispatch in `crates/voom-cli/src/main.rs`:

```rust
use voom_cli::cli::{..., SchedulerCommand, ...};
use voom_cli::commands::{..., scheduler, ...};

Command::Scheduler(ref command) => dispatch_scheduler(&cli, command.clone()).await,
```

Add `dispatch_scheduler` to `main.rs`:

```rust
async fn dispatch_scheduler(cli: &Cli, command: SchedulerCommand) -> Result<Exit> {
    let cfg = match resolve_cfg(cli) {
        Ok(cfg) => cfg,
        Err(err) => {
            voom_cli::envelope::emit_err("scheduler", err.code(), err.to_string(), None, None)?;
            return Ok(Exit::Failure);
        }
    };
    let local = Local {
        db_url: cfg.database_url.clone(),
        config_path: cfg.config_path.display().to_string(),
    };
    Ok(Exit::from_run_code(
        scheduler::run(&cfg.database_url, local, command).await?,
    ))
}
```

- [ ] **Step 5: Run CLI command unit tests**

Run:

```bash
cargo test -p voom-cli scheduler
```

Expected: scheduler command unit tests pass.

- [ ] **Step 6: Commit CLI command implementation**

Run:

```bash
git add crates/voom-cli/src/cli.rs crates/voom-cli/src/main.rs crates/voom-cli/src/commands/mod.rs crates/voom-cli/src/commands/scheduler.rs crates/voom-cli/src/commands/scheduler_test.rs
git commit -m "feat: inspect scheduler decisions from cli"
```

## Task 7: CLI Integration Snapshots

**Files:**
- Create: `crates/voom-cli/tests/scheduler_envelope.rs`
- Create: `crates/voom-cli/tests/snapshots/scheduler_envelope__*.snap`

- [ ] **Step 1: Add failing integration tests**

Create `crates/voom-cli/tests/scheduler_envelope.rs`:

```rust
#![expect(
    clippy::unwrap_used,
    reason = "integration tests favor unwrap over plumbing Result<()> through every assertion"
)]

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::json;
use serde_json::Value;
use tempfile::NamedTempFile;
use time::OffsetDateTime;
use voom_core::{NodeId, TicketId, WorkerId};
use voom_store::repo::scheduler_decisions::{
    NewSchedulerDecision, SchedulerDecisionKind, SchedulerDecisionOutcome, SchedulerDecisionRepo,
    SchedulerReasonCode, SchedulerRequestSource, SqliteSchedulerDecisionRepo,
};

#[tokio::test]
async fn scheduler_decisions_list_outputs_envelope() {
    let fixture = fixture().await;

    let assert = Command::cargo_bin("voom")
        .unwrap()
        .arg("--database-url")
        .arg(&fixture.url)
        .args(["scheduler", "decisions", "list", "--worker-id", "2"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"command\":\"scheduler\""));

    let mut json: Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    redact_local(&mut json);
    insta::assert_json_snapshot!("scheduler_decisions_list_outputs_envelope", json);
}

#[tokio::test]
async fn scheduler_decisions_show_outputs_full_explanation() {
    let fixture = fixture().await;

    let assert = Command::cargo_bin("voom")
        .unwrap()
        .arg("--database-url")
        .arg(&fixture.url)
        .args([
            "scheduler",
            "decisions",
            "show",
            "--decision-id",
            &fixture.decision_id.to_string(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"explanation_json\""));

    let mut json: Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    redact_local(&mut json);
    insta::assert_json_snapshot!("scheduler_decisions_show_outputs_full_explanation", json);
}

struct Fixture {
    url: String,
    decision_id: u64,
    _tmp: NamedTempFile,
}

async fn fixture() -> Fixture {
    let tmp = NamedTempFile::new().unwrap();
    let url = voom_store::test_support::sqlite_url_for(tmp.path());
    voom_store::init(&url).await.unwrap();
    let pool = voom_store::connect(&url).await.unwrap();
    seed_refs(&pool).await;
    let repo = SqliteSchedulerDecisionRepo::new(pool);
    let decision = repo
        .create(NewSchedulerDecision {
            decision_kind: SchedulerDecisionKind::LeaseAcquire,
            request_source: SchedulerRequestSource::RemoteAcquire,
            idempotency_key: Some("idem".to_owned()),
            request_node_id: Some(NodeId(1)),
            request_worker_id: Some(WorkerId(2)),
            ticket_id: Some(TicketId(3)),
            selected_worker_id: Some(WorkerId(2)),
            selected_node_id: Some(NodeId(1)),
            selected_lease_id: None,
            outcome: SchedulerDecisionOutcome::Selected,
            reason_code: SchedulerReasonCode::Selected,
            summary: "selected worker 2 for ticket 3".to_owned(),
            candidate_count: 1,
            selected_score: Some(1700),
            suppression_key: None,
            explanation: json!({"scoring_version":1,"candidates":[]}),
            now: OffsetDateTime::UNIX_EPOCH,
        })
        .await
        .unwrap();

    Fixture {
        url,
        decision_id: decision.id,
        _tmp: tmp,
    }
}

fn redact_local(json: &mut Value) {
    if let Some(local) = json.get_mut("local") {
        local["db_url"] = json!("[db-url]");
        local["config_path"] = json!("[config-path]");
    }
}

async fn seed_refs(pool: &sqlx::SqlitePool) {
    sqlx::query(
        "INSERT INTO nodes \
         (id, name, kind, status, registered_at, last_seen_at, heartbeat_ttl_seconds, \
          auth_token_hash, auth_token_hint, metadata) \
         VALUES (1, 'node-1', 'remote', 'active', '1970-01-01T00:00:00Z', \
                 '1970-01-01T00:00:00Z', 60, 'hash', 'hint', '{}')",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO workers (id, name, kind, status, node_id, registered_at, last_seen_at) \
         VALUES (2, 'worker-2', 'remote', 'active', 1, \
                 '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z')",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO tickets \
         (id, job_id, kind, state, priority, payload, attempt, max_attempts, \
          next_eligible_at, created_at, state_changed_at) \
         VALUES (3, NULL, 'probe_file', 'ready', 0, '{}', 0, 3, \
                 '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z', \
                 '1970-01-01T00:00:00Z')",
    )
    .execute(pool)
    .await
    .unwrap();
}
```

- [ ] **Step 2: Run integration tests and review snapshots**

Run:

```bash
cargo test -p voom-cli --test scheduler_envelope
cargo insta review
```

Expected: tests pass after accepting intentional snapshots. Snapshot JSON must have one envelope on stdout with no stderr log lines mixed into stdout.

- [ ] **Step 3: Commit CLI snapshots**

Run:

```bash
git add crates/voom-cli/tests/scheduler_envelope.rs crates/voom-cli/tests/snapshots
git commit -m "test: snapshot scheduler decision cli envelopes"
```

## Task 8: Idempotency Replay And Suppression Coverage

**Files:**
- Modify: `crates/voom-control-plane/src/cases/remote_execution_test.rs`
- Modify: `crates/voom-store/src/repo/scheduler_decisions_test.rs`

- [ ] **Step 1: Add replay test for unchanged decision id**

Add to `remote_execution_test.rs`:

```rust
#[tokio::test]
async fn remote_acquire_replay_returns_original_scheduler_decision_without_rescoring() {
    let fixture = remote_fixture(&[(OP, vec!["shared_mount"])], &[OP], &[]).await;
    fixture.ready_ticket(OP).await;

    let first = fixture
        .cp
        .remote_acquire(fixture.acquire_input("replay-decision", "hash-replay-decision"))
        .await
        .unwrap();
    let replay = fixture
        .cp
        .remote_acquire(fixture.acquire_input("replay-decision", "hash-replay-decision"))
        .await
        .unwrap();

    assert_eq!(replay, first);
    let decision_count = fixture
        .cp
        .scheduler_decisions()
        .list(Default::default())
        .await
        .unwrap()
        .len();
    assert_eq!(decision_count, 1);
}
```

- [ ] **Step 2: Add suppression test with time bucket**

Add to `scheduler_decisions_test.rs`:

```rust
#[tokio::test]
async fn suppression_key_keeps_selected_rows_separate_from_idle_rows() {
    let (repo, _tmp) = repo().await;
    let selected = repo.create(selected_input()).await.unwrap();
    let mut idle = selected_input();
    idle.decision_kind = SchedulerDecisionKind::Idle;
    idle.outcome = SchedulerDecisionOutcome::Idle;
    idle.reason_code = SchedulerReasonCode::NoReadyTicket;
    idle.suppression_key = Some("remote_acquire:worker:5:no_ready_ticket:0".to_owned());
    idle.ticket_id = None;
    idle.selected_worker_id = None;
    idle.selected_node_id = None;
    idle.selected_score = None;
    repo.create_or_suppress(idle.clone()).await.unwrap();
    repo.create_or_suppress(idle).await.unwrap();

    let rows = repo.list(SchedulerDecisionFilter::default()).await.unwrap();

    assert_eq!(rows.len(), 2);
    assert!(rows.iter().any(|row| row.id == selected.id));
    assert!(rows.iter().any(|row| row.suppressed_count == 1));
}
```

- [ ] **Step 3: Run targeted tests**

Run:

```bash
cargo test -p voom-control-plane remote_acquire_replay_returns_original_scheduler_decision_without_rescoring
cargo test -p voom-store suppression_key_keeps_selected_rows_separate_from_idle_rows
```

Expected: both tests pass.

- [ ] **Step 4: Commit replay and suppression coverage**

Run:

```bash
git add crates/voom-control-plane/src/cases/remote_execution_test.rs crates/voom-store/src/repo/scheduler_decisions_test.rs
git commit -m "test: cover scheduler decision replay and suppression"
```

## Task 9: Sprint 9 Closeout Documentation

**Files:**
- Create: `docs/superpowers/specs/2026-05-24-voom-sprint-9-closeout.md`

- [ ] **Step 1: Create closeout matrix after implementation verification**

Create `docs/superpowers/specs/2026-05-24-voom-sprint-9-closeout.md`:

```markdown
---
name: voom-sprint-9-closeout
description: Sprint 9 closeout evidence for scheduler scoring, durable decisions, remote acquire integration, and CLI inspection.
status: complete
date: 2026-05-24
sprint: 9
references:
  - docs/superpowers/specs/2026-05-24-voom-sprint-9-design.md
---

# VOOM Sprint 9 Closeout

## Acceptance Matrix

| Requirement | Evidence |
|---|---|
| Reusable scheduler scoring core | `cargo test -p voom-scheduler` |
| Hard eligibility gates | `cargo test -p voom-scheduler scorer_rejects_hard_gate_failures_with_reason_codes` |
| Fixed weights and scoring version | `crates/voom-scheduler/src/lib.rs`, `cargo test -p voom-scheduler` |
| Deterministic tie-breaking | `cargo test -p voom-control-plane remote_acquire_uses_scored_priority_then_tie_breaker` |
| Worker-level concurrency enforcement | `cargo test -p voom-control-plane remote` |
| Node-level concurrency enforcement | `cargo test -p voom-control-plane node_default_limit_blocks_second_concurrent_remote_acquire` |
| Durable scheduler decision persistence | `cargo test -p voom-store scheduler_decisions` |
| Idle/no-candidate suppression | `cargo test -p voom-store suppression_key_keeps_selected_rows_separate_from_idle_rows` |
| Selected and rejected explanations | `cargo test -p voom-scheduler` and `cargo test -p voom-control-plane remote_acquire_no_candidate_is_success_with_decision` |
| Remote acquire scorer integration | `cargo test -p voom-control-plane remote_acquire` |
| Idempotent replay without rescoring | `cargo test -p voom-control-plane remote_acquire_replay_returns_original_scheduler_decision_without_rescoring` |
| Artifact access scoring vocabulary | `cargo test -p voom-scheduler` and `cargo test -p voom-control-plane remote` |
| CLI list/show output | `cargo test -p voom-cli --test scheduler_envelope` |
| Full local CI | `just ci` |

## Deferred Work

Sprint 9 intentionally leaves daemon scheduling loops, scheduling windows,
dynamic throttles, UI controls, production metrics, real media transfer cost
modeling, and policy-configurable scoring weights to the later roadmap phases
named in `docs/specs/voom-control-plane-design.md`.
```

- [ ] **Step 2: Commit closeout document**

Run:

```bash
git add docs/superpowers/specs/2026-05-24-voom-sprint-9-closeout.md
git commit -m "docs: close out sprint 9 scheduler scoring"
```

## Task 10: Full Verification

**Files:**
- No file changes expected unless verification finds defects.

- [ ] **Step 1: Run targeted verification**

Run:

```bash
cargo test -p voom-scheduler
cargo test -p voom-store scheduler_decisions
cargo test -p voom-control-plane remote
cargo test -p voom-api remote_execution_route
cargo test -p voom-cli scheduler
cargo test -p voom-cli --test scheduler_envelope
```

Expected: every command exits 0.

- [ ] **Step 2: Run full CI**

Run:

```bash
just ci
```

Expected: `fmt-check`, `lint`, `check-test-layout`, `test`, `doc`, `deny`, and `audit` all pass. If `cargo audit` reports a real advisory, do not call Sprint 9 complete; record the advisory and resolve or escalate it.

- [ ] **Step 3: Inspect git state**

Run:

```bash
git status --short --branch
git log --oneline -10
```

Expected: worktree clean on `feat/sprint-9`; recent commits show the Sprint 9 implementation series.
