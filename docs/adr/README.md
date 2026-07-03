# Architecture Decision Records

Each ADR captures one architecturally significant decision: its context, the
decision, its consequences, and the alternatives considered and rejected. They
are append-only — supersede an ADR with a new one rather than rewriting history.

| ADR | Title |
|---|---|
| [0001](0001-durable-jobs-over-events.md) | Durable jobs route work; events record facts |
| [0002](0002-out-of-process-workers-only.md) | All providers are out-of-process workers from day one |
| [0003](0003-sqlx-and-tokio-foundation.md) | sqlx + tokio as the async storage foundation |
| [0004](0004-sibling-unit-tests.md) | Sibling unit tests with `cargo-llvm-cov` and SonarCloud |
| [0005](0005-plan-phase-entry-point.md) | Per-phase planner entry point `plan_phase` |
| [0006](0006-workflow-summary-schema.md) | Durable workflow-summary schema and repository shape |
| [0007](0007-phase-barrier-coordinator.md) | Phase-barrier coordinator owns one job and drives the existing executor |
| [0008](0008-per-phase-report-regenerated-against-refreshed-facts.md) | Per-phase compliance report is regenerated against post-commit refreshed facts |
| [0009](0009-resume-opens-new-job-reconciles-prior-rows.md) | Resume opens a new job and reconciles against the prior job's per-(file, phase) rows |
| [0010](0010-compliance-report-job-read-mode.md) | `compliance report` gains a read-only `--job-id` post-run mode |
| [0011](0011-audio-transcode-plannability-vs-preservation.md) | Audio-transcode plannability does not gate on per-stream preservation facts |
| [0012](0012-paused-time-db-pool-guard.md) | Guard against pairing tokio paused time with the real SQLite pool in tests |
| [0013](0013-payload-evolution-contract.md) | Durable JSON payloads evolve under a deny-unknown-fields contract |
| [0014](0014-database-error-source-chain.md) | Preserve the `sqlx::Error` source chain in `VoomError::Database` |
| [0015](0015-control-plane-module-decomposition.md) | Decompose oversized control-plane modules along cohesion seams |
| [0016](0016-worker-protocol-exact-version-match.md) | Worker protocol enforces an exact version match, no skew window |
| [0017](0017-verify-artifact-dsl-operation.md) | `verify artifact` compiles and plans, execution wiring deferred |
| [0018](0018-terminal-failure-issue-auto-open.md) | Terminal-failure tickets auto-open a `terminal_failure` issue in the transition transaction |
| [0019](0019-commit-gate-lineage-commit-check.md) | Lineage-commit safety-gate check runs in the prepare transaction |
| [0020](0020-eac3-audio-transcode-target.md) | E-AC-3 audio transcode target and deterministic audio bitrate |
| [0021](0021-language-filter-untagged-and-zero-match-semantics.md) | Language-filter semantics for untagged tracks and zero-match keeps |
| [0022](0022-sidecar-role-classification.md) | Stem-prefix sidecar classification for V1 asset ingest |
| [0023](0023-filter-addressed-track-defaults-and-ordering.md) | Filter-addressed track defaults, track-level ordering, and forced flag |
| [0024](0024-malformed-media-and-hardlink-facts.md) | Malformed-media failure class and hardlink inode facts |
| [0025](0025-backup-worker-and-backup-before-mutation-gate.md) | Real backup worker, durable backup records, and a backup-before-mutation gate |
| [0026](0026-audio-synthesis-downmix.md) | Audio track synthesis (downmix companion) |
| [0027](0027-library-root-and-scan-configuration.md) | Library and library-root configuration, root-scoped scan, and fail-closed disabled roots |
| [0028](0028-scheduling-and-safety-policy-crud.md) | Scheduling policy and safety policy CRUD, and the fail-closed safety gate |
| [0029](0029-external-system-registration-health-and-sync.md) | External-system registration, health, path mappings, and read-only sync |
| [0030](0030-issue-action-cli.md) | Issue action CLI: operator read + transition surface |
| [0031](0031-keyset-cursor-pagination.md) | Keyset cursor pagination for durable-row inspection commands |
| [0032](0032-video-and-quality-scoring-profile-management.md) | Video profile and quality-scoring profile management |
