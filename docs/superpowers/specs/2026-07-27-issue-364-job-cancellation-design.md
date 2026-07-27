# Operator Job Cancellation Design

**Issue:** #364
**Status:** Approved
**Base:** `main` at `882eadd4569447e9ee568fbae8a71b23f241937c`

## Goal

Let an operator cancel an open job through the standard CLI envelope and make
that cancellation an effective stop-scheduling boundary. Preserve durable
ticket evidence, prevent any new lease after cancellation wins, and report
missing and terminal jobs explicitly.

## Review charter

- **Outcome:** `voom job cancel --job-id <id> --reason <text>` atomically
  cancels an open job, records the reason, and prevents its unleased work from
  dispatching.
- **Permitted surface:** job CLI parsing and handler; the existing
  control-plane cancellation case; job transition error classification;
  ready-ticket selection; guarded lease acquisition; focused store,
  control-plane, and CLI tests; operator runbook; ADR 0046; and this issue's
  design and plan.
- **Direct dependencies:** `ControlPlane::cancel_job`,
  `SqliteJobRepo::cancel_in_tx`,
  `SqliteTicketRepo::ready_for_operations_in_tx`,
  `SqliteLeaseRepo::acquire_in_tx`, the standard CLI envelope, the injected
  control-plane clock, and #343's store-owned worker eligibility gate.
- **Compatibility:** no schema, ticket-state vocabulary, event taxonomy,
  worker request, policy grammar, or existing CLI wire shape changes. The new
  success payload uses the existing job projection.

Explicit exclusions:

- Held leases are not preempted. The worker protocol has no abort request.
- Cancellation does not rewrite pending or ready tickets as failed.
- No new cancelled ticket state or ticket-cancelled event is introduced.
- This issue does not add bulk job cancellation or a resume command.
- #338, #339, #367, #368, and #369 remain outside this campaign issue.

An excluded concern is blocking if the implementation depends on it or claims
to solve it.

## Current behavior

`ControlPlane::cancel_job` updates an open job to `cancelled` and appends the
strict `job.cancelled` payload in one transaction. It accepts blank reasons.
`SqliteJobRepo` reports both a missing row and a terminal row as `CONFLICT`.

The CLI exposes `job list` and `job show` only. Store ready-ticket selection
filters on ticket state, time, attempts, and operation without considering the
parent job. Lease acquisition repeats the ticket gates but also ignores the
job. Consequently a ready ticket can acquire a lease after its job is
cancelled.

## Decision

### CLI contract

`JobCommand` gains:

```text
Cancel {
    job_id: u64,
    reason: String,
}
```

The handler opens the control plane through the existing common path and calls
`cancel_job(JobId(job_id), reason, cp.clock().now())`. Success emits:

```json
{
  "schema_version": 1,
  "command": "job",
  "status": "ok",
  "data": {
    "job": {
      "id": 1,
      "kind": "policy_execute",
      "state": "cancelled",
      "priority": 0,
      "created_at": "...",
      "updated_at": "...",
      "epoch": 1
    }
  }
}
```

The existing envelope helper supplies `local` and warnings consistently with
job show. No second stdout line, human confirmation, or stderr success text is
added.

Clap owns missing-argument and malformed-ID failures, which remain `BAD_ARGS`
and exit 1. Runtime failures use `emit_voom_error`, retain one envelope, and
exit 2.

### Reason validation and error precedence

`ControlPlane::cancel_job` calls `require_audit_field("reason", &reason)`
before beginning the write transaction. Direct callers and the CLI therefore
cannot append a cancellation event with a blank reason.

The CLI must open the configured database before it can invoke the use case,
so database-open errors precede reason validation. Once the control plane is
open, reason validation precedes transaction acquisition.

`SqliteJobRepo` retains the conditional open-state update. If it affects no
rows, it reads the job state inside the same transaction:

- no row returns `VoomError::NotFound("job <id>")`;
- an existing terminal row returns `VoomError::Conflict` naming the current
  state and rejected transition.

This classification applies to all job terminal transitions and makes missing
job cancellation explicit without introducing a CLI-only preflight race.

### Atomic cancellation

The control plane transaction contains:

1. the conditional open-to-cancelled job update;
2. the canonical job reread;
3. one `job.cancelled` append containing the exact reason; and
4. commit.

Any update, reload, event, or commit error rolls the transaction back.
Terminal, missing, and blank-reason attempts append no cancellation event and
change no ticket or lease row.

### Store-owned scheduling gate

Ready-ticket selection returns a ticket only when:

```text
ticket.job_id is null
or the referenced job exists with state = open
```

The foreign key guarantees an assigned parent exists under normal schema
operation. Expressing the rule with `EXISTS` also fails closed if corrupt
state is observed.

The guarded ready-to-leased update repeats the same condition. It remains one
SQL transition with ticket state, time, retry, and parent-job gates. This is
the authoritative check because the job may be cancelled after candidate
selection. A zero-row result remains `CONFLICT`, with the diagnostic expanded
to name parent-job eligibility.

The check composes with #343 inside its savepoint-protected lease acquisition.
Worker eligibility and parent-job eligibility must both pass before a lease
row and lease/ticket events commit. If either fails, the savepoint restores
the ticket to ready and no lease or event row survives.

### Concurrency

Job cancellation and lease acquisition both mutate SQLite state. The
ready-to-leased statement reads the parent job as part of its guarded update.
SQLite's writer serialization gives two allowed outcomes:

- lease acquisition commits first: the operation already owns a held lease,
  and later cancellation does not preempt it; or
- cancellation commits first: the parent-job predicate fails and no new lease
  or lease/ticket event is created.

Tests exercise the persisted post-cancellation boundary. They do not assert an
unreliable task schedule between two concurrent futures.

### Durable ticket behavior

Cancellation does not mutate ticket rows. Pending and ready states, attempt
counts, epochs, results, and dependency rows remain exact audit evidence.
Selection tests prove a cancelled job's ready ticket is absent while a
jobless ready ticket and an open job's ready ticket remain present.

Lease tests prove direct acquisition for a cancelled parent fails atomically:
the ticket remains ready with its attempt and epoch unchanged, no lease row is
created, and no `lease.acquired` or `ticket.leased` event is appended.

An already-held lease may complete. Any dependent ticket can become ready but
cannot cross the lease gate while the job is cancelled.

## Compatibility and rollback

The new CLI subcommand is additive. Existing list and show envelopes are
unchanged. Cancellation reuses the existing job JSON projection and public
error codes.

No database or durable JSON shape changes. Existing jobs, tickets, leases, and
events remain readable by old and new binaries. A rollback removes the CLI and
scheduling check; it needs no data migration. Operators must not intentionally
roll back while relying on cancellation to suppress queued work, because an
older binary lacks that safety gate.

## Security and operations

The reason is durable operator-supplied audit text. Empty or whitespace-only
values fail before a transaction begins. The command does not log credentials,
call a network service, or expose new filesystem access.

The success envelope includes the resulting state and epoch. Operators can use
existing `job show`, `ticket list`, `event list`, and lease inspection commands
to verify the durable outcome. The runbook states that cancellation stops new
leases but does not kill already-held work.

## Test strategy

- Parser tests require both arguments and preserve the standard `BAD_ARGS`
  envelope for omissions.
- CLI integration tests cancel an open job and inspect the success envelope,
  job row, unchanged ready ticket, and exact cancellation event reason.
- CLI tests cover missing, each terminal state, and whitespace reason. They
  inspect job, ticket, lease, and event tables after every failure.
- Control-plane tests prove blank reasons fail before transaction work and
  successful cancellation atomically updates the job and appends one event.
- Store job tests distinguish missing from terminal transitions.
- Ready-selection tests cover open, cancelled, and jobless tickets.
- Lease tests call acquisition directly after cancellation and assert no
  ticket mutation, lease row, or lease/ticket event.
- An existing held lease test proves cancellation leaves that lease and its
  leased ticket unchanged.
- Sensitivity checks temporarily remove reason validation, the candidate
  parent-job filter, and the atomic acquisition filter; each named behavior
  test must fail.

## Success criteria

- The command emits exactly one standard JSON envelope.
- Open jobs become cancelled and append one reason-bearing event.
- Missing jobs are `NOT_FOUND`; terminal jobs are `CONFLICT`; blank reasons are
  `CONFIG_INVALID`.
- Cancellation failures persist no partial job, ticket, lease, or event state.
- No ticket linked to a cancelled job can be selected or newly leased.
- Jobless and open-job tickets retain their previous scheduling behavior.
- Held leases are unchanged and the operator limitation is documented.
- Focused tests, lint, hooks, and `just ci` pass without warnings.
