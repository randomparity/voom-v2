# Real subprocess crash-recovery stress design

## Scope and goal

Issue #606 extends the merged distributed stress harness with an opt-in real-process path. A
configured fraction of first attempts must be acquired by supervised `chaos-worker` subprocesses,
terminate while holding their leases, and then be expired, reassigned, and settled exactly once.
Every child must be reaped on success and on returned-error cleanup. Production ticketing, API,
schema, and public protocol behavior do not change.

[ADR 0095](../../adr/0095-preseed-stress-recovery-with-process-crashed-attempts.md) governs the
process-prelude topology and retry ownership. ADR 0094 continues to govern the stress session,
recovery gate, and conservation oracle.

## Configuration

Add `VOOM_STRESS_PROCESS_CRASH_PERCENT`, default `0`, accepted range `0..=25`. It is independent
of the existing in-process stall/crash mix, but all three percentages must total at most 25. Zero
preserves the existing harness exactly. A non-zero value selects
`max(1, tickets * percent / 100)` first attempts, capped below the ticket count only by the
validated percentage. Each selected attempt receives `max_attempts = 2`, as today.

The existing `just stress` recipe remains the single entry point. Operators enable the new arm
with the environment variable; it remains excluded from `just ci`. The effective configuration
and final report include the requested percentage, selected process count, every process
observation, total attempts, and retries.

## Components and data flow

### Process crash runner

Add a focused process-backed entry point beside `RemoteSyntheticRunner`. It consumes the existing
API runner configuration, the `chaos-worker` binary path supplied by the integration harness, and
one seeded ticket whose payload selects chaos crash mode. It performs these ordered steps:

1. Activate exactly one worker on its dedicated registered node and retain the returned worker ID
   and epoch.
2. Spawn `chaos-worker` on loopback with an ephemeral port, credentials derived from the activated
   identity, piped stdin/stdout, inherited stderr, and `kill_on_drop(true)`.
3. Read one bounded `BOUND addr=...` readiness line, retain the remaining child handle, and mark
   the worker ready through the API.
4. Acquire one lease through the API and construct the worker-protocol `OperationRequest` from the
   acquired lease without flattening its typed IDs.
5. Dispatch over the real worker socket. Accept only a connection termination paired with the
   child exiting non-zero; an ordinary terminal response, no lease, readiness timeout, or a child
   that exits before dispatch is an error.
6. Await the child, remove it from the supervisor registry, and append one abandoned
   `ExecutionRecord` plus one `ProcessCrashObservation` containing PID, node ID, worker ID, ticket
   ID, lease ID, and exit status.

The runner never calls complete, fail, or lease heartbeat after dispatch. The lease remains held
until the existing control-plane recovery path expires it.

### Child supervisor

The integration harness owns one supervisor for the whole prelude. Every spawn is registered
before readiness is awaited. A successful crash is explicitly waited and removed. Any error runs
`shutdown_all`: close retained stdin, wait up to five seconds, issue kill for a still-running
child, then wait up to five seconds for reap. Cleanup accumulates child-specific errors and still
attempts every child. After either normal completion or cleanup, the registry must be empty.

The supervisor is test-only and accepts the binary path explicitly; it never searches `PATH`.
All bind addresses are literal loopback with port zero. Child stdout is drained only through the
single readiness line because crash mode emits no later stdout contract; stderr is inherited so a
failure remains diagnosable.

### Stress orchestration

Seed the workload before starting synthetic sessions. The selected process tickets are independent
roots placed first in deterministic seed order and carry the chaos crash payload. Start and reap
their dedicated workers sequentially, recording the acquired leases. Then create the ordinary
synthetic sessions and enter the existing drain loop. Its first recovery wave snapshots the
process-crashed leases, advances the injected domain clock past their actual deadlines, and
requires the recovery report's expired set to match them. Synthetic workers acquire each retry and
settle it. Process observations are converted to abandoned execution records before the unchanged
conservation check runs.

## Failure and cleanup contract

- Invalid configuration fails before database, server, or process creation.
- Missing or non-executable binary, malformed readiness, readiness timeout, early clean exit,
  acquisition without a lease, unexpected worker terminal output, or an unreaped child returns an
  error naming the node/process phase.
- Once a child has spawned, every exit path invokes supervisor cleanup before returning. Server
  and synthetic-session tasks retain the existing stop/join/abort ordering.
- A process observation is recorded only after the child has been explicitly waited; therefore an
  observed crash is also evidence of reap.
- Recovery must expire every and only recorded process-crashed lease in its recovery wave.
- Existing conservation still requires one terminal ticket event, one terminal event per lease,
  attempt/log/event agreement, no held lease, and no duplicate non-abandoned execution.

## Security model

The design adds only a local test boundary. The local operator controls the environment percentage
and Cargo-provided binary path; neither is remotely supplied. Configuration is integer-bounded
before use. The executable path is passed directly to `Command` with fixed arguments and fixed
environment keys, never through a shell. Worker sockets and the API server bind only to loopback.
Per-process secrets are generated in memory, passed only through the child environment and worker
protocol credentials, and never printed. Child output is treated as untrusted: the readiness line
is bounded by a five-second deadline and must parse as a loopback socket address. This test does
not defend against a malicious locally substituted build artifact or a hostile local operator.

## Verification

1. Unit-test configuration bounds and deterministic process-count selection.
2. Add an ignored integration test selected by `just stress`; with a small non-zero process
   percentage, first observe it fail before the process runner exists, then implement it.
3. Test cleanup with a deliberately returned post-spawn error and prove the supervisor registry is
   empty and the child PID is no longer waitable through its retained handle.
4. Run a small real-process cell and assert process count, non-zero exits, recorded lease identity,
   exact expiry, reassignment, terminal settlement, and zero supervised children.
5. Inject a duplicate terminal observation into the reused conservation input, observe the
   existing duplicate diagnostic, revert, and rerun green.
6. Run focused `voom-fakes` tests, `just stress` with the process arm, then `just ci`. Record the
   first real-process run configuration, duration, counts, and findings in the pull request.

## Durable workflow checkpoint

- Branch: `feat/real-subprocess-crash-recovery-606`
- Base branch: `main`
- Scope token: `q606-bc84e057`
- Architecture: host `x86_64`; target architectures `none declared`; relationship
  `no-target-declared`
- Guardrails: `cargo test -p voom-fakes`; `just stress`; `just fmt-check`; `just lint`;
  `just check-test-layout`; `just test`; `just ci`
- Open findings and deferrals: none.
