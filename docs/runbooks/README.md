# Operator Runbooks

Operator procedures for running and recovering a VOOM installation from the
`voom` CLI. Each runbook exercises part of the durable control-plane surface
proven daemon-ready in the Sprint 17 closeout matrix
(`docs/superpowers/specs/2026-07-02-voom-sprint-17-closeout.md`).

| Runbook | What it covers | Matrix rows it exercises |
|---|---|---|
| [operator-real-media-execution.md](operator-real-media-execution.md) | End-to-end real-media run: `init` → `worker run-local` (ffmpeg + mkvtoolnix) → `scan` → `policy create` → `policy input create-from-scan --all` → `compliance execute` → inspect / resume. Includes the sample-policy catalog, output/promotion rules, mid-run monitoring (WAL concurrent reads), and the `run-local` two-line stdout contract. | Node/worker grants, policies, input sets, artifacts, reports, recovery records (closeout matrix 1); continuous monitoring, scan reconciliation, crash recovery, event/inspection (matrix 2). |
| [migration-rollback.md](migration-rollback.md) | Rolling a VOOM install back to a prior release: up-only migrations, binary-before-DB ordering, WAL-aware snapshot restore, dirty-migration recovery, and the no-backup fallbacks. | The durability/recovery foundation the daemon relies on before it automates any mutation (recovery records, reports; matrix 1). |

## How the runbooks relate to the daemon-readiness matrix

The closeout matrix maps every Sprint 18-20 daemon input, policy, action, and
recovery path to a CLI/API command and a test. These runbooks are the
*operator-facing* narrative of that same surface:

- The **operator-real-media-execution** runbook is the human procedure behind the
  operator execution e2e test
  (`crates/voom-cli/tests/operator_execution_e2e.rs`) and the `run-local`
  lifecycle tests. It is how an operator drives the node/worker-grant, policy,
  input-set, artifact, and report state families before any daemon automates
  them.
- The **migration-rollback** runbook is the recovery story for the durable state
  the daemon consumes: it documents how to restore and verify the database the
  daemon reads, so a daemon sprint never has to invent a rollback path.

When a daemon sprint (18-20) automates one of these procedures, it must consume
the same durable state and CLI/API contracts these runbooks use — it must not
introduce a new operator path that bypasses them.
