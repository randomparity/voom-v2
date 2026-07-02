# 0018 — Terminal-failure tickets auto-open a `terminal_failure` issue in the transition transaction

## Status

Accepted

## Context

The architectural spec (`docs/specs/voom-control-plane-design.md` → Issue Model
and Error Handling And Recovery → Failure taxonomy) is normative: when a ticket
transitions to `failed` terminally, the host must open exactly one
`terminal_failure` issue "in the same transaction that records the terminal
transition", linked to the ticket and its last lease, with severity and priority
derived from the failure's `FailureClass`. This is the dead-letter-queue analogue
— a terminal failure must not vanish into the append-only event log.

None of that happened before this change:

- The issues repository (`crates/voom-store/src/repo/policy/issues.rs`) handled
  only the `policy_noncompliant` kind. Its `upsert_policy_noncompliant_in_tx`
  hardcodes `kind='policy_noncompliant'`, `severity='medium'`,
  `priority='normal'`, `priority_source='policy'`.
- `FailureClass::issue_severity` / `issue_priority`
  (`crates/voom-core/src/taxonomy/failure.rs`) existed but were unused.
- All four `TicketFailedTerminal` payload sites set `issue_id: None`
  (`cases/execution/tickets.rs`; three sites in `cases/execution/leases.rs`).
- The DB was already prepared: the `issues.kind` CHECK constraint includes
  `'terminal_failure'` (migration 0004) and `issues.dedupe_key` has a partial
  UNIQUE index (migration 0008).

## Decision

Add a dedicated, idempotent store method rather than generalizing the
`policy_noncompliant` upsert into a parameterized upsert.

`SqliteIssueRepo::open_terminal_failure_in_tx(tx, draft, now) -> IssueId`:

- INSERTs one row with `kind='terminal_failure'`, `priority_source='system'`,
  `status='open'`, and `severity` / `priority` / `priority_reason` / `title` /
  `body` supplied by the caller.
- Idempotency is keyed on the existing partial-unique `issues.dedupe_key`. The
  dedupe key is `terminal_failure:ticket:{ticket_id}`. A ticket transitions to
  `failed` at most once (the state machine treats `failed` as terminal and every
  terminal UPDATE is guarded on the pre-terminal state), so this key yields
  exactly one issue per terminal transition. On a UNIQUE conflict the method
  returns the existing issue id without inserting a duplicate row or new links —
  a safety net for a retried transaction, not an expected path.
- Inserts `issue_links` rows: one `link_type='ticket'` for the ticket, and, when
  a lease exists, one `link_type='lease'` for the last lease. The pre-lease
  selection-failure path has no lease, so the lease link is `Option`.

The control plane owns the policy: at each of the four `TicketFailedTerminal`
sites it derives severity/priority via `FailureClass::issue_severity` /
`issue_priority`, calls `open_terminal_failure_in_tx` inside the existing
transaction, and populates `TicketFailedTerminal.issue_id` with the returned id.
The shared derivation lives in one `ControlPlane::open_terminal_failure_issue_in_tx`
helper so the four sites cannot drift.

The `policy_noncompliant` path is left intact and untouched (AGENTS.md Rule 4 /
Rule 7).

## Consequences

- Terminal failures become durable, queryable issues linked to their ticket and
  lease; `TicketFailedTerminal.issue_id` is now always populated.
- No new migration: the schema was already provisioned (CHECK + dedupe_key
  unique). No new durable JSON payload column, so the ADR 0013
  deny-unknown-fields inventory is unchanged.
- `terminal_failure` issues are opened but never auto-resolved by this path.
  Resolution is an operator/policy action (issue-action CLI, T14 / #283) — out
  of scope here.
- The two issue kinds now use two distinct store methods (upsert vs. idempotent
  insert) because their semantics genuinely differ: `policy_noncompliant`
  re-evaluates and mutates an existing open issue on every compliance run;
  `terminal_failure` is opened exactly once and never revisited by this path.

## Considered & rejected

- **Generalize `upsert_policy_noncompliant_in_tx` into one parameterized upsert.**
  Rejected: the update-existing-fields / toggle-status logic is meaningful only
  for the recurring compliance evaluation. A terminal transition happens once, so
  upsert semantics are dead weight and would blur two different contracts into one
  method (AGENTS.md Rule 3). A shared row-decode helper is enough reuse.
- **Emit a separate `issue.opened` event.** Rejected: the durable link is the
  `issue_id` on `TicketFailedTerminal`; a second event would duplicate a fact the
  payload already records (ADR 0001 — events record facts, one per fact).
- **Dedupe on `ticket_id + attempt` or `ticket_id + lease_id`.** Rejected as
  unnecessary: `failed` is reached at most once per ticket, so `ticket_id` alone
  already guarantees exactly-once; a wider key adds nothing and complicates the
  pre-lease (no-lease) path.
- **Open the issue in a follow-up transaction after the transition commits.**
  Rejected: the spec requires the issue in the same transaction so a crash cannot
  leave a terminal ticket with no issue.
