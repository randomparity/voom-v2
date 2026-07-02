---
status: accepted
date: 2026-07-02
deciders: [VOOM core]
---

# 0028 — Scheduling policy and safety policy CRUD, and the fail-closed safety gate

## Context

The pre-daemon safety baseline (design doc → *Security And Safety*) forbids any
daemon loop from auto-scheduling real media mutation until operators can
configure and inspect durable **scheduling policy** and **safety policy** records
from the CLI. The schema inventory reserves `scheduling_policies` and
`safety_policies`, but neither existed: no tables, no repos, no CLI, and
`compliance execute` read no safety configuration.

The safety policy gates every future automation decision. Its fields are fixed by
the spec: which operation kinds may auto-execute, whether backup is required
before a mutation, whether approval is required, which commit modes are allowed
(`add_only` before replace/delete/archive), what verification level is required
before commit, and whether failed / partial / recovery-required records block
later automation. Reads are **fail-closed**: a missing, stale, or insufficient
policy records a blocked issue rather than synthesizing a default.

ADR 0025 (#278) added a backup-before-mutation execute hook gated on an explicit
`backup_root` input and named T12 (#281) as the future owner of the durable
"backup required" trigger. This ADR is that owner.

Two facts constrain the gate:

- **The execute path is add-only today** (ADR 0025): remux/transcode/audio
  workers never overwrite their input and commit installs via hard-link + temp,
  so the only commit mode the path produces is `add_only`.
- **There is no daemon and no approval subsystem.** Scheduling policy has no
  reader; `approval_required` has no grant path.

## Decision

**Two `STRICT` tables (migration 0020), two repos, two CLI command trees, and an
opt-in fail-closed gate in `compliance execute`.**

### Schema

`scheduling_policies` and `safety_policies` each carry an integer id, a unique
`slug`, a `display_name`, a `schema_version` (fail-closed staleness), and
`created_at` / `updated_at`. Fields are enumerated in the linked spec.
`scheduling_policies` models only the parseable, enforceable subset of the
design's `schedule` example. `safety_policies` stores `auto_execute_operations`
and `allowed_commit_modes` as JSON arrays of scalar enum wire-strings; the repo
validates every element against the enum vocabulary on write (fail-loud), so the
ADR-0013 `deny_unknown_fields` payload contract — which governs JSON columns
deserialized into structs — does not apply.

### Fail-closed staleness via `schema_version`

Each safety-policy row records the `SAFETY_POLICY_SCHEMA_VERSION` the writing
binary used. The gate treats any row whose version ≠ the reading binary's current
version as stale and blocks. This is the concrete meaning of the spec's "too old,
or lacks a field required by an operation": a binary that adds a required field
bumps the constant, and prior rows become stale until re-created — never silently
defaulted.

### The gate is opt-in and enforced at existing hooks

`compliance execute` enforces a safety policy only when
`--safety-policy <slug>` is supplied. Absent it, behavior is unchanged (manual
operator flows and their snapshots are unaffected); a future daemon always
supplies one. When supplied, the gate evaluates the named policy against the
generated plan and the run options and blocks on: missing policy, stale
`schema_version`, `approval_required`, `add_only` not in `allowed_commit_modes`,
`backup_required` with no `--backup-root`, a required `verification_level` with
no `verify_artifact` node in the plan, any planned mutating operation absent from
`auto_execute_operations`, a failed backup for a targeted file version when
`block_on_failed_records`, and a recovery-required commit for a targeted file
version when `block_on_recovery_required_records`. `backup_required` with a
supplied `--backup-root` threads the root into the ADR-0025 backup-before-
mutation gate — the wire from safety policy to the T9 hook.

On any block the gate opens one durable `policy_noncompliant` issue (dedupe key
`safety_blocked:v1:policy=<slug>:pv=<id>:is=<id>`) enumerating the reasons and
returns `VoomError::PolicyValidationError`; nothing is dispatched.

## Consequences

- The daemon-readiness baseline gains its two required durable config families
  with full CLI CRUD and JSON envelopes, and `compliance execute` has a real,
  tested fail-closed safety read at every hook that exists today.
- The `backup_required` → `--backup-root` → ADR-0025 gate wire is live.
- Enforcement of scheduling policy, of `approval_required` beyond an
  unconditional block, and of any commit mode past `add_only` lands with the
  daemon / destructive-automation work (T20 / Sprint 28), reading the same rows.
- A binary upgrade that bumps `SAFETY_POLICY_SCHEMA_VERSION` invalidates existing
  safety rows by design; operators re-create them. This is the fail-closed
  contract, not a regression.

## Considered & rejected

- **Enforce the gate on every `compliance execute`.** Rejected: it would break
  every existing manual-execute flow and snapshot and conflate operator-initiated
  execution with daemon auto-execution. The spec's fail-closed rule targets the
  daemon; the opt-in flag models exactly that while keeping the manual path
  intact.
- **A distinct `blocked` issue status / a new `safety_blocked` issue kind.**
  Rejected: the existing `policy_noncompliant` open issue with a distinct dedupe
  prefix records the block durably with no new migration or vocabulary.
- **A third `block_on_partial_records` field.** Rejected as a phantom field: no
  durable record status is distinct from *failed* and *recovery-required*; a
  partial run leaves one or both, so two booleans cover the spec's intent.
- **Rich scheduling-policy fields (`prefer local_gpu_for`, `cloud_egress_budget`).**
  Rejected as speculative: no subsystem consumes them pre-daemon.
- **Storing operation/commit-mode sets as child tables.** Rejected: JSON arrays
  of validated enum strings match the existing durable-JSON-column pattern and
  need no join for a whole-policy read.
