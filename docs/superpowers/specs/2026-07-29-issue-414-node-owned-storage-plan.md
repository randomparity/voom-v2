# Issue #414 — Node-owned storage documentation plan

## Base and guardrails

- Base: `main` at `d8f84892d67c61ba674464ab5120521205c33fd0`
- Branch: `docs/node-owned-storage-414`
- Focused checks:
  - inspect changed ADR paths against `origin/main`
  - search every required authority, lifecycle, failure, recovery, non-goal,
    supersession, and roadmap term
  - `just fmt-check`
- Full check: `just ci`
- Delivery: one documentation PR closing #414

The documents form one architecture contract. Do not merge or publish an
intermediate state in which the new ADR conflicts with an unamended accepted
ADR or the original roadmap.

## Step 1 — Establish the governing architecture

Files:

- `docs/adr/0050-node-owned-storage-and-byte-blind-control-plane.md`
- `docs/superpowers/specs/2026-07-29-issue-414-node-owned-storage-design.md`

Write the accepted authority model and its implementation-facing design. Define
logical nodes versus incarnations, single-owner roots, provider-relative
locations, the byte-blind boundary, scan sessions, owner-local scheduling,
full-operation liveness, distributed commit fencing, ambiguous recovery,
security, observability, compatibility, and non-goals.

Expected red before the change:

- no accepted ADR defines a node agent or testable byte-blind boundary;
- no design defines scan-session terminal states or distributed commit recovery;
- `rg -n "byte-blind|provider-relative|recovery_required" docs/adr
  docs/superpowers/specs` finds no governing complete contract.

Verification:

- read the ADR and design independently against issue #414 and epic #413;
- confirm every durable state has authority, entry, terminal, and recovery
  behavior;
- confirm current behavior is identified as pending implementation rather than
  documented as already shipped.

Commit boundary: hold with Step 2; the governing ADR must not land without
amending conflicting accepted documents.

## Step 2 — Amend the original specification and prior decisions

Files:

- `docs/specs/voom-control-plane-design.md`
- `docs/adr/0019-commit-gate-lineage-commit-check.md`
- `docs/adr/0025-backup-worker-and-backup-before-mutation-gate.md`
- `docs/adr/0027-library-root-and-scan-configuration.md`
- `docs/adr/0034-policy-tool-requirements-use-worker-capabilities.md`
- `docs/adr/README.md`

Update the original architecture and roadmap without rewriting implementation
history. Add the node-agent authority and byte boundary to the architecture
sections; replace global path/prefix assumptions in the target storage model;
define owner-local artifact resolution and commit; and map the work to Sprints
7–10 plus the scan-session portion of Sprint 18.

Add explicit later-decision notes:

- ADR 0019 preserves safety-gate authority while owner nodes perform mutation;
- ADR 0025 preserves backup-before-mutation while owner nodes perform backup;
- ADR 0027 is superseded only for canonical-path global identity and prefix
  scoping;
- ADR 0034 is superseded only for excluding authenticated remote owner workers;
  and
- ADRs 0048 through 0050 appear in the ADR index.

Expected red before the change:

- the original spec still permits control-plane hashing and describes concrete
  paths as globally meaningful;
- ADR 0027 rejects root-linked locations;
- ADR 0034 rejects every remote concrete provider;
- ADRs 0019 and 0025 do not distinguish coordination host from storage owner.

Verification:

- `rg -n "ADR 0050|node agent|provider-relative|byte-blind|owner node|scan
  session" docs/specs/voom-control-plane-design.md docs/adr`;
- inspect every edited historical ADR to ensure its retained decision remains
  explicit;
- inspect Sprints 7, 8, 9, 10, and 18 for the delivery mapping and exclusions.

Commit boundary: one `docs:` commit containing Steps 1 and 2 so the accepted
documentation is internally coherent.

## Step 3 — Adversarial review and verification

Files:

- all files changed in Steps 1 and 2

Run the changed-ADR audit against `origin/main`. Review ADR 0050 first, then the
design, then this plan. Challenge unclear authority, invalid state transitions,
unsafe recovery, path leakage, hidden transfer assumptions, speculative S3
behavior, compatibility shims, and claims that unimplemented work exists.

Review the final diff for duplication and contradictions. Preserve necessary
detail where it makes a distributed safety boundary testable; remove prose that
does not affect implementation or verification.

Expected red before review:

- at least one ambiguous phrase such as "host-owned" can assign mutation to the
  wrong machine or leave a failure state without an authority.

Verification:

- `just fmt-check`
- `just ci`
- `git diff --check origin/main...HEAD`
- review the final GitHub diff after push

Commit boundary: any defensible review corrections are a separate `docs:` fix
commit only if the primary commit was already created; otherwise fold them into
the single coherent documentation commit.

## Step 4 — Track and ship

Transition #414 from `status:in-progress` to `status:in-review`, push the branch,
and open a PR with `Closes #414`. Record the complete `WORK:REVIEW` block on the
PR and a `WORK:TRAJECTORY` block on the issue. Wait for required checks, resolve
only defensible findings, transition to `status:awaiting-merge`, and merge under
the campaign authorization.

After merge, verify #414 is closed, remove any stale status label, update the
campaign manifest, refresh `main`, and revalidate the base before starting
#415.
