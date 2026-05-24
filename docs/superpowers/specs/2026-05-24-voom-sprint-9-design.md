---
name: voom-sprint-9-design
description: Sprint 9 design for reusable scheduler scoring, durable scheduler decision logs, remote acquire integration, and CLI decision inspection.
status: proposed
date: 2026-05-24
sprint: 9
references:
  - docs/specs/voom-control-plane-design.md
  - docs/superpowers/specs/2026-05-24-voom-sprint-8-design.md
---

# VOOM Sprint 9 - Scheduler Scoring

## 1. Purpose

Sprint 9 closes the remote-scheduling MVP by making scheduler decisions
deterministic, explainable, durable, and inspectable. The sprint introduces a
reusable scoring core in `voom-scheduler`, wires remote lease acquisition
through that scorer, records scheduler decisions in a first-class table, and
adds CLI inspection commands for operators and agents.

Sprint 9 is still a synthetic scheduling milestone. It does not introduce real
media execution, daemon scheduling loops, production metrics, UI controls, real
artifact transfer, or policy-configurable scoring weights.

## 2. Scope

Sprint 9 delivers:

- A reusable scheduler scoring core in `voom-scheduler`.
- Scoring inputs for ready tickets, workers, nodes, grants, capabilities,
  active lease counts, node health, and artifact access plans.
- Hard eligibility gates for capability, grants, denies, node and worker
  status, heartbeat freshness, worker capacity, node capacity, and supported
  artifact access.
- Fixed scoring weights for health, artifact access mode, synthetic
  locality/cost factors, spare capacity, and deterministic tie-breakers.
- Node-level concurrency limits enforced transactionally during acquire.
- Worker-level concurrency enforcement preserved and routed through the scored
  acquire path.
- Durable `scheduler_decisions` records for selected, rejected, idle, and
  no-candidate outcomes.
- Idle/no-candidate suppression so polling remains debuggable without flooding
  the decision table.
- A remote acquire integration that records the decision and acquires the lease
  in one transaction.
- Remote acquire idempotency that replays the original decision outcome without
  rescoring.
- Agent-facing CLI inspection commands for scheduler decisions.
- Deterministic scoring, decision-log, concurrency, and remote acquire tests.
- A small roadmap clarification in `docs/specs/voom-control-plane-design.md`
  that names the work intentionally deferred beyond Sprint 9.

Sprint 9 explicitly does not deliver:

- Real media execution.
- Daemon scheduling loops, scheduling windows, or dynamic throttles.
- Production metrics endpoints or trace-export infrastructure.
- Web UI scheduler views or controls.
- Real artifact transfer, object-store integration, or production cost
  accounting.
- Policy-configurable scoring weights.
- Production TLS or remote-network hardening beyond the Sprint 8 boundary.

## 3. Architecture

The Sprint 9 scheduling path is:

```text
remote acquire request
  -> node-token authenticated control-plane use case
  -> ready ticket and candidate worker/node snapshot
  -> voom-scheduler scoring core
  -> durable scheduler_decisions row
  -> transactional lease acquire
  -> artifact access plan selection
  -> remote acquire response with decision id
```

`voom-scheduler` owns scoring rules and explanation construction.
`voom-control-plane` owns remote acquire orchestration, authentication,
idempotency, transaction boundaries, and calls into the scheduler. `voom-store`
owns the `scheduler_decisions` repository and migration. `voom-cli` owns the
agent-facing inspection surface.

The scorer returns both a selected candidate and a structured explanation. The
explanation must be suitable for durable storage and CLI output; route code
must not reconstruct decision reasons from logs or events.

Events may reference scheduler decision ids, but the full decision is durable
product data in `scheduler_decisions`. This follows the architecture rule that
events record facts and durable tables own operational state.

## 4. Scoring Model

Scoring is deterministic and conservative. A scored candidate is a
`ticket + worker + node + artifact access mode` tuple. The scorer may rank
multiple ready tickets for the authenticated worker, and the reusable
`voom-scheduler` core must also support multi-worker fixtures so later daemon
scheduling can reuse the same model.

Hard gates remove invalid candidates before soft scoring:

- worker lacks the required capability;
- worker lacks an execution grant for the operation;
- worker is explicitly denied for the operation;
- worker is retired or otherwise not executable;
- worker is linked to a retired, stale, or heartbeat-expired node;
- worker has no supported artifact access mode for the dispatch;
- worker-level active leases are at or above the operation limit;
- node-level active leases are at or above the node limit.

Eligible candidates are scored with fixed Sprint 9 weights:

- health and heartbeat freshness;
- artifact access mode compatibility derived from the ticket payload and worker
  advertised access modes;
- synthetic locality and cost factors derived from the same access-mode
  candidate data that will become the selected artifact access plan after lease
  acquisition;
- spare worker capacity;
- spare node capacity;
- deterministic tie-breaker.

The exact numeric constants can be implementation details, but they must be
reported in each decision explanation under a `scoring_version`. Tests should
lock expected ordering and representative factor values, not every incidental
constant if doing so would make harmless tuning noisy.

Tie-breaking must be stable. When two candidates have equal scores, selection
orders by ticket priority descending, `next_eligible_at` ascending, node id,
worker id, and ticket id.

## 5. Concurrency

Concurrency is an enforcement rule, not only a scoring factor. Sprint 9 must
prevent concurrent acquires from racing past worker or node limits.

Worker-level limits continue to come from execution grants. Node-level limits
live in a dedicated scheduler-owned table keyed by `node_id`, with a Sprint 9
`max_parallel_leases` value and timestamps. A missing node limit row means the
node uses the Sprint 9 default limit of one active execution lease. Tests and
fixture setup may create explicit rows with higher limits when they need to
prove multi-worker node capacity. This avoids overloading node registration
metadata and keeps scheduler policy queryable without parsing opaque JSON.

The remote acquire transaction must:

1. authenticate and validate idempotency;
2. snapshot ready tickets and candidate workers/nodes;
3. score candidates;
4. prepare the scheduler decision explanation;
5. check worker and node active lease counts;
6. acquire the lease;
7. record the scheduler decision with the selected lease id;
8. create the selected artifact access plan.

If a candidate wins scoring but loses the final capacity check because another
transaction acquired first, the acquire path may rescore a bounded number of
times inside the transaction or fail the attempt as no candidate. The behavior
must be deterministic and covered by concurrent acquire tests.

## 6. Durable Decision Logs

Sprint 9 adds a `scheduler_decisions` table. The table should be structured
enough for filtering and JSON-backed enough to preserve the full explanation
without over-normalizing the first version.

Required fields:

```text
id
created_at
updated_at
first_seen_at           -- first equivalent idle/no-candidate occurrence
last_seen_at            -- most recent equivalent idle/no-candidate occurrence
decision_kind          -- lease_acquire, idle, no_candidate
request_source         -- remote_acquire initially
idempotency_key        -- nullable, for remote acquire traceability
request_node_id        -- nullable request context, present for remote acquire
request_worker_id      -- nullable request context, present for remote acquire
ticket_id              -- nullable for idle/no ready ticket
selected_worker_id     -- nullable
selected_node_id       -- nullable
selected_lease_id      -- nullable until lease acquire succeeds
outcome                -- selected, idle, no_eligible_candidate, rejected
reason_code            -- stable short machine-readable reason
summary                -- short human-readable string
candidate_count
selected_score         -- nullable integer
suppressed_count       -- number of equivalent idle/no-candidate attempts folded into this row
suppression_key        -- nullable key used for idle/no-candidate dedupe
explanation_json       -- full candidate scoring/rejection details
```

Required indexes:

```text
created_at DESC
ticket_id
request_worker_id
request_node_id
selected_worker_id
selected_node_id
outcome
reason_code
suppression_key
```

Sprint 9 decision vocab:

```text
decision_kind:
  lease_acquire
  idle
  no_candidate

outcome:
  selected
  idle
  no_eligible_candidate
  rejected

minimum reason_code values:
  selected
  no_ready_ticket
  missing_capability
  missing_grant
  operation_denied
  worker_not_executable
  node_not_executable
  heartbeat_expired
  unsupported_artifact_access
  worker_capacity_full
  node_capacity_full
```

The explanation JSON shape is intentional:

```json
{
  "scoring_version": 1,
  "operation": "probe_file",
  "weights": {
    "capability": 1000,
    "health": 500,
    "artifact_access": 100,
    "locality": 20,
    "cost": 20,
    "tie_breaker": 1
  },
  "candidates": [
    {
      "worker_id": 12,
      "node_id": 4,
      "eligible": true,
      "score": 1574,
      "factors": {
        "capability": 1000,
        "health": 500,
        "worker_capacity": 50,
        "node_capacity": 20,
        "artifact_access": 4,
        "tie_breaker": 0
      },
      "reasons": []
    }
  ]
}
```

Rejected candidates must include reason codes. Idle/no-candidate decisions must
also carry enough explanation to distinguish no ready tickets from ready tickets
with no eligible candidate.

Idle/no-candidate suppression happens at write time. The suppression key should
include the request source, worker or node context when present, reason code,
operation set, and a time bucket. A suppressed write increments
`suppressed_count`, `updated_at`, and `last_seen_at` instead of inserting a new
row. Selected decisions are never suppressed.

## 7. Remote Acquire Integration

Sprint 8 remote acquire selected work by deterministic ready-ticket ordering
for a single authenticated worker. Sprint 9 replaces that selection with
scorer-backed selection while preserving Sprint 8's safety rules:

- node-token authentication remains mandatory;
- worker-to-node ownership remains mandatory;
- retired or stale nodes cannot acquire new work;
- workers must advertise the operation;
- workers must have a grant and must not be denied;
- unsupported artifact access modes fail visibly;
- no ready work is a non-error idle outcome;
- route idempotency remains scoped to the authenticated node, route, worker,
  and request hash.

The acquire response should include the scheduler decision id for every
non-error outcome: leased, idle, and no-candidate. A same-key/same-body retry
replays the original outcome and decision id without creating a new decision row
or rescoring. A same-key different-body retry remains rejected without mutating
scheduling state.

Candidate breadth stays controlled. Sprint 9 remote acquire remains
worker-scoped to preserve Sprint 8's runner contract: a runner requests work
for one authenticated worker and can only receive a lease for that worker. The
scorer may rank multiple ready ticket candidates for that worker, and the node
limit still protects node-level capacity across concurrent worker-scoped
acquires. Cross-worker node-agent dispatch and cross-node global scheduling wait
for daemon scheduling.

## 8. CLI Inspection

Sprint 9 adds a small CLI surface:

```text
voom scheduler decisions list
voom scheduler decisions show <decision-id>
```

Initial filters for `list`:

```text
--ticket-id <id>
--worker-id <id>
--node-id <id>
--outcome <selected|idle|no_eligible_candidate|rejected>
--limit <n>
```

`list` returns compact decision rows. `show` returns the full decision record
including `explanation_json`. Both commands must emit the standard single JSON
envelope on stdout and stable public error codes.

For `list`, `--worker-id` and `--node-id` match both request context and
selected ids so idle/no-candidate rows remain discoverable.

The CLI exists because scheduler decisions are agent-facing operational data.
Operators and agents should not need direct database access to answer why work
was or was not leased.

## 9. Error Handling

Sprint 9 keeps Sprint 8 error behavior unless implementation proves a new code
is necessary.

- No ready work: non-error idle outcome, durable decision logged with
  suppression.
- Ready work but no eligible candidate: non-error no-candidate outcome when
  the worker or node should keep polling.
- Invalid node, worker, token, idempotency, or ownership: existing `NOT_FOUND`,
  `CONFLICT`, `BAD_ARGS`, or runtime error codes.
- Replay: return the original outcome and scheduler decision id without
  rescoring.
- Database or unexpected internal failures: existing runtime/internal codes.

Events should not carry full scheduler explanations. If an event is emitted for
a scheduler decision, it references the durable scheduler decision id.

## 10. Testing And Acceptance

Sprint 9 acceptance is durable and externally inspectable:

- Scored candidate ordering is deterministic under fixtures.
- Selected candidates include expected factor scores and reason trails.
- Rejected candidates are visible in `scheduler_decisions`.
- Idle and no-candidate attempts are logged with suppression counts.
- Remote acquire uses scorer-backed selection.
- Remote acquire idempotency replay returns the original scheduler decision.
- Worker concurrency limits cannot be exceeded under concurrent acquire
  attempts.
- Node concurrency limits cannot be exceeded under concurrent acquire attempts.
- Scheduler decisions link to selected lease, ticket, worker, and node when a
  lease is acquired.
- CLI `scheduler decisions list` emits compact valid JSON envelopes.
- CLI `scheduler decisions show` emits the full structured explanation.
- The project spec clarifies Sprint 9 deferrals.

Required verification:

```text
cargo test -p voom-scheduler
cargo test -p voom-store scheduler_decisions
cargo test -p voom-control-plane remote
cargo test -p voom-cli scheduler
just ci
```

Exact test names may shift during implementation, but targeted scheduler,
store, control-plane, and CLI tests plus full `just ci` are required before
closeout.

## 11. Closeout Matrix

Sprint 9 closeout must record evidence for:

- reusable scheduler scoring core;
- hard eligibility gates;
- fixed scoring weights and `scoring_version`;
- deterministic tie-breaking;
- worker-level concurrency enforcement;
- node-level concurrency enforcement;
- durable scheduler decision persistence;
- idle/no-candidate suppression;
- selected and rejected candidate explanations;
- remote acquire scorer integration;
- remote acquire idempotent replay without rescoring;
- artifact access mode scoring using the Sprint 8 access-plan vocabulary and
  selected plan evidence;
- CLI decision list/show output;
- explicit deferral of daemon loops, UI controls, production metrics,
  real-media transfer cost modeling, and policy-configurable scoring weights.

The sprint is not complete until this evidence is repeatable from tests and the
closeout document links each acceptance item to a verification command or
fixture.
