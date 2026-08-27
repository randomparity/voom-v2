# Transaction openers — first run

Issue: [#552](https://github.com/randomparity/voom-v2/issues/552)
Design: [2026-08-26-transaction-openers-design.md](2026-08-26-transaction-openers-design.md)
ADR: [0086](../../adr/0086-transaction-openers-are-named-helpers.md)
Governing rule: [ADR 0083](../../adr/0083-read-then-write-transactions-begin-immediate.md)

Issue #552 criterion 3 asks for the check's first workspace run and a
disposition for every finding. There is no allow file to defer into, so every
finding below is fixed.

## The first run

`scripts/check-transaction-openers.sh` against `main` (570d7c54), before any
conversion:

```
check-transaction-openers: pool-level transactions opened outside voom_store::tx
  ... 63 sites ...
```

| crate | sites |
|---|---:|
| `voom-store` | 60 |
| `voom-control-plane` | 3 |

**63 openers, but 194 transactions.** That gap is the finding. Nine of those 63
sites were shared-helper *definitions* — `voom-store`'s seven spellings of
`begin`, and `voom-control-plane`'s `begin_tx` / `begin_immediate_tx`. So 54
transactions opened directly, and the other 140 reached the database through one
of those nine, taking whichever mode the helper's original author chose rather
than one their own body justified. ADR 0083 described `voom-store` as having
"three spellings of this transaction"; it had seven.

The check cannot see those. It reports the boundary, and deleting the shared
helpers is what forced each transaction to state its own shape.

## Final census

194 production transactions, each opened by a named helper. Zero remain outside
`voom_store::tx`.

| helper | `voom-store` | `voom-control-plane` | total |
|---|---:|---:|---:|
| `begin_read_then_write` | 25 | 53 | 78 |
| `begin_write_first` | 49 | 48 | 97 |
| `begin_read_only` | 12 | 5 | 17 |
| `begin_serialized_read` | 0 | 2 | 2 |
| **total** | **86** | **108** | **194** |

A further 24 openers live in test files, which the check excludes.

## Findings

### 1. Twenty-three read-then-write transactions on a deferred `BEGIN` — the #546 defect

Each reads before it writes, so each was refused at its first write without ever
consulting `busy_timeout`. All now open with `begin_read_then_write`.

`voom-store` (8):

| site | reads first |
|---|---|
| `SqliteLeaseRepo::fail` | ADR 0083's deferred list |
| `SqliteLeaseRepo::force_release` | ADR 0083's deferred list |
| `SqliteTicketRepo::mark_ready_if_unblocked` | ADR 0083's deferred list |
| `SqliteSchedulerDecisionRepo::create` | validates with a `SELECT` |
| `SqliteSchedulerDecisionRepo::create_or_suppress` | validates with a `SELECT` |
| `delete_library` | `SELECT EXISTS` before `DELETE` |
| `create_library_root` | reads the root by slug |
| `artifact_access_plans::create_selected` | reads the plan |

`voom-control-plane` (15):

| site | reads first |
|---|---|
| `force_release_lease` | ADR 0083's deferred list — `SELECT … JOIN` probe |
| `create_library_root` | reads before insert |
| `assign_library_root_owner` | reads before update |
| `activate_library_root` | reads before update |
| `mark_library_root_unavailable` | reads before update |
| `retire_library_root` | reads before update |
| `record_discovered_file` | the alias path validates before writing |
| `reconcile_rename` | reads before writing |
| `accept_identity_evidence` | reads before writing |
| `create_file_version` | reads before writing |
| `acquire_use_lease` | probes scope liveness |
| `heartbeat_use_lease` | `SELECT` then `UPDATE` |
| `enforce_safety_policy` | reads before writing |
| `register_worker_for_node` | `auth_record_in_tx` |
| `create_missing_tickets` | reads before writing |

`record_discovered_file` is worth naming: it branches, and only the alias-proof
path reads first. The rule takes the worst case across paths, so it is
read-then-write even though its other branch inserts immediately.

Remote-node use cases form a class of their own — every one authenticates via
`auth_record_in_tx`, a `SELECT`, before it acts. Eleven openers sit behind that
fence; the eight already `IMMEDIATE` were correct, `register_worker_for_node` was
not, and two turned out to be read-only.

**Seven of these were invisible to the census.** `delete_library`,
`create_library_root` and five sibling sites went through a file-local `begin()`
in `repo/library/mod.rs`, so nothing listed them as openers. They surfaced only
as compile errors after that helper was deleted. The worklist was incomplete;
deleting the helper is what caught it.

### 2. `ControlPlane::require_selected_version_still_active` — missed by the census

`workflow/coordinator/finalize.rs` called `crate::cases::begin_tx(...)` fully
qualified, which the census regex (`= begin_tx(`) did not match. It surfaced as a
compile error when the helper was deleted. Read-only; now `begin_read_only`.

The general lesson is the same as finding 1: the guarantee the check provides is
"no pool-level opener outside `voom_store::tx`", not "no other named helper
exists". Deleting the helpers is what closes that gap.

### 3. A fourth shape the design did not anticipate

`WorkflowExecutor::create_guarded_root_tickets` opened its planned-lineage guard
with `BEGIN IMMEDIATE` despite only reading. That read as a redundant mode, and
it was converted to `begin_read_only`.

`guarded_root_dispatch_waits_for_promoter_then_rejects_every_root` failed:
expected `StaleIdentityEvidence`, got `NoEligibleWorker`. In WAL a reader does
not block on an uncommitted writer, so the plain `BEGIN` guard read the
pre-promotion snapshot, passed, and dispatched a superseded run. The
`BEGIN IMMEDIATE` was load-bearing.

That is a fourth shape — read-only, but ordered after in-flight writers — and it
gets its own name, `begin_serialized_read`. `remote_open_commit_intents` takes it
for the same reason. ADR 0086 records the decision.

The reasoning that produced the wrong conversion is worth naming: the guard and
the ticket creation are separate transactions, so `BEGIN IMMEDIATE` appeared to
close no window. It does not close the check-then-act window — but it does close
the stale-snapshot one, which is what the test was pinning.

### 4. Nine transactions dropped from `BEGIN IMMEDIATE` to a deferred `BEGIN`

Each was verified by reading to write on its first statement, where both modes
take the write lock at the same moment and wait identically. Seven were
`IMMEDIATE` only because they shared a helper with read-then-write callers —
precisely the information loss the vocabulary removes, working in the other
direction.

| site | first statement |
|---|---|
| `phase_a_gate_abort_with_event` | `INSERT INTO commit_intents` |
| `create_document_with_version` | three writes, no reads |
| `recover_commit` (recovery-required mark) | `mark_recovery_required_in_tx` |
| `abort_prepared_after_hook_failure` | `mark_aborted_in_tx` |
| `record_verification_started` | `INSERT OR IGNORE INTO workers` |
| `record_verification_started_for_worker` | event append |
| `ensure_policy_verifier` | `INSERT OR IGNORE INTO workers` |
| `create_guarded_root_tickets` (ticket tx) | `create_ticket_in_tx` |
| `heartbeat_lease_observed` | `UPDATE leases` |

`ensure_policy_verifier` is the one worth flagging: "ensure" reads as
read-then-write, and it is not — `register_builtin_if_missing_in_tx` opens with
`INSERT OR IGNORE`. Reading found that; the name would have misled.

### 5. `heartbeat_lease_observed` unpicks a deliberate decision

Commit `8981da5e` ("serialize heartbeat and audio prepare writers") set this
transaction to `BEGIN IMMEDIATE` on purpose, and two tests pinned it. Its first
statement is `UPDATE leases`, so the `IMMEDIATE` is redundant: a deferred `BEGIN`
requests the write lock at that same `UPDATE` and waits on `busy_timeout`
identically.

Both tests asserted internal mechanism rather than behaviour:

- `heartbeat_reserves_writer_at_transaction_start` asserted *which opener*
  reported the contention. It now asserts that contention is reported and the
  lease is unmoved, and is renamed `heartbeat_fails_cleanly_behind_a_held_writer`.
- `heartbeat_serializes_one_production_attempt_behind_a_writer` asserted an
  observer flag set immediately after the opener returned — a moment a deferred
  `BEGIN` reaches at once. The flag proved nothing once the wait moved to the
  `UPDATE`, so it and its two assertions are gone. Its load-bearing assertions
  are untouched: one production attempt, blocked rather than failed, succeeding
  after release.

This is the change in the branch most worth a second opinion. It is recorded
here rather than buried in a diff because it removes a guarantee someone chose.

## Criterion 2 — revert and observe

Reverting #546 must redden a check. The opener check cannot do it: it sees a
helper call either way, because the revert changes *which* helper. ADR 0086
accepts that trade. The tests carry it instead.

With `expire_due` opened by `begin_write_first` in place of
`begin_read_then_write`, in each crate independently:

| test | result | evidence |
|---|---|---|
| `expire_due_asks_for_the_write_lock_at_its_opener` | **fails** | `expire_due must contend at its opener, not at a later statement: database error: lease expire: … (code: 5) database is locked` |
| `expire_due_waits_out_a_concurrent_writer` | **fails** | caught at probe 1–2: `lease expire: … database is locked` |

Restored, all four pass, and each survives `just test-repeat … 20` with no
failure.

**The ordered sequence alone was not sufficient, which was measured rather than
predicted.** The design specified one control probe followed by one
`is_finished()` check. Against the revert that *passed*: the treatment had not
yet reached its first `UPDATE`, the writer was released, and it then ran
uncontended — the same false green the 200 ms sleep produced, by a different
route. Two changes fix it, both described in the design:

- the waits-out test signals from inside the spawned task and probes four times,
  checking after each; and
- `expire_due_asks_for_the_write_lock_at_its_opener` removes concurrency
  entirely — under a zero `busy_timeout` the error's context names the opener
  that asked for the lock, which depends on nothing but the ordering SQLite
  guarantees.

The first draft of that second test had a vacuity bug of its own: a 60-second
cutoff against a 60-second TTL, and `expires_at < ?` is strict, so nothing was
due and the assertions passed without the opener ever requesting the lock. It now
asserts that an uncontended re-run really does expire the lease.

## Residual

The waits-out tests still cannot prove the treatment reached its `BEGIN` while
the lock was held; nothing exposes "this connection is waiting". They fail toward
a green treatment. Owned by issue
[#588](https://github.com/randomparity/voom-v2/issues/588).
`expire_due_asks_for_the_write_lock_at_its_opener` is not subject to it.

The check proves deliberateness, not correctness: a caller can still pick
`begin_write_first` for a body that reads first. The failure mode is a visible
wrong claim at one call site, reviewable in a diff, rather than an invisible
omission. ADR 0086 states why that trade is accepted.

## Follow-up filed

Seventeen `begin_read_only` transactions open a transaction to run reads, and
most run a single one — `node_ticket_exists` is one statement between `BEGIN` and
`COMMIT`. A lone read needs no transaction. Naming the openers is what made that
visible. Removing them is a separate change and nothing here depends on it:
issue [#589](https://github.com/randomparity/voom-v2/issues/589).
