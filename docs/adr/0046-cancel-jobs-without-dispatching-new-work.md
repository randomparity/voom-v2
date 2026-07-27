---
status: accepted
date: 2026-07-27
deciders: [VOOM core]
---

# 0046 — Cancel jobs without dispatching new work

## Context

The control plane can transition an open job to `cancelled` and append a
`job.cancelled` event, but operators cannot invoke that use case through the
CLI. More importantly, cancellation currently changes only the job row.
Ready tickets remain visible to remote candidate selection and can still
acquire leases. The local workflow executor also reaches lease acquisition
directly. A CLI that reports successful cancellation while new work can still
be dispatched would expose a misleading operational contract.

Ticket rows have no cancelled state. Their existing pending and ready states
are durable workflow evidence, and changing that vocabulary would require a
migration plus changes to event and inspection contracts. The worker protocol
also has no request for revoking work that already owns a held lease.

Design: [operator job cancellation design][design].

[design]: ../superpowers/specs/2026-07-27-issue-364-job-cancellation-design.md

## Decision

### Expose cancellation through the existing job command

Add:

```text
voom job cancel --job-id <id> --reason <text>
```

The command validates that the reason is not empty or whitespace, then invokes
the existing control-plane cancellation use case using the control plane's
clock. Success emits one standard `command: "job"` JSON envelope containing
the cancelled job projection. It uses the existing public error codes and exit
contracts:

- a missing job is `NOT_FOUND` with exit code 2;
- a terminal job is `CONFLICT` with exit code 2;
- a blank reason is `CONFIG_INVALID` with exit code 2; and
- clap syntax failures remain `BAD_ARGS` with exit code 1.

Cancellation is an open-to-cancelled transition. The job update and its one
`job.cancelled` event commit atomically. A failed cancellation changes no job,
ticket, lease, or event row.

### Treat the parent job as a scheduling gate

Pending and ready tickets retain their states for audit and inspection.
Store-owned ready-ticket selection excludes a ticket when it has a parent job
whose state is not open. Tickets without a parent job remain eligible.

Lease acquisition repeats the parent-job condition in the same guarded ticket
transition that changes ready to leased. This is the authoritative dispatch
gate for local execution, remote execution, and callers that bypass candidate
selection. Candidate selection is an optimization and inspection boundary;
the atomic lease transition is the correctness boundary.

SQLite serializes the job cancellation and ticket lease writes. Whichever
write commits first defines the outcome: a lease acquired first is existing
work, while a cancellation committed first prevents that ticket from
acquiring a new lease.

### Do not preempt held work

Cancellation stops new dispatch. It does not force-release a held lease or
send a worker-side abort because no such worker protocol exists. A held
operation can finish and persist its ticket result after the parent job is
cancelled. Its downstream pending or ready tickets remain durable but cannot
acquire a lease.

An operator who needs to terminate a stuck held lease can use the existing
lease force-release controls. Adding coordinated worker abort is a separate
protocol and lifecycle decision.

## Consequences

- The CLI reports a cancellation only when the durable job transition and
  audit event both committed.
- A cancelled job can retain pending or ready tickets. Inspection therefore
  preserves why work existed without implying it remains schedulable.
- Every new dispatch path remains protected because it must acquire a lease
  through the store gate.
- Already-held work is not interrupted. Cancellation has a precise
  stop-scheduling meaning rather than an unimplemented kill guarantee.
- There is no migration, new ticket state, new event, or worker wire change.
- Rollback is a code revert. Rows created by the new version use existing
  persisted shapes and remain readable by the previous version.

## Considered and rejected alternatives

### Add only the CLI command

Rejected. The current store would continue selecting and leasing ready tickets
from the cancelled job.

### Mark every pending and ready ticket failed

Rejected. Failure and cancellation are different facts, and the ticket schema
has no cancelled state. Rewriting every row would lose the distinction,
require misleading failure events, and complicate concurrent dependency
transitions.

### Add a cancelled ticket state

Rejected. Stopping dispatch needs only a parent-job eligibility gate. A new
durable state and event taxonomy would be larger than the operator outcome and
would not solve already-held work.

### Force-release or abort every held lease

Rejected. Force release does not stop a worker that already received the
request, and the worker protocol has no cancellation transport. Claiming
preemption would create two writers for the same operation without a complete
fencing design.
