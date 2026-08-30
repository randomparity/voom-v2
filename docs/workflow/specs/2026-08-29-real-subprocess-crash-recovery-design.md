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

Add a crate-private process-backed entry point beside `RemoteSyntheticRunner`; it is implementation
support for the in-crate stress harness, not a new externally consumable `voom-fakes` API. It consumes the existing
API runner configuration, the `chaos-worker` binary path supplied by the integration harness, and
one seeded ticket whose payload selects chaos crash mode. It performs these ordered steps:

1. Activate exactly one worker on its dedicated registered node and retain the returned worker ID
   and epoch.
2. Spawn `chaos-worker` on loopback with an ephemeral port, credentials derived from the activated
   identity, piped stdin/stdout, inherited stderr, and `kill_on_drop(true)`.
3. Receive one `BOUND addr=...` readiness observation from the supervisor within five seconds and
   at no more than 4 KiB including the newline, then mark the worker ready through the API. Missing
   newline at the byte limit, extra bytes in the frame, timeout, or a non-loopback address is
   malformed readiness. The supervisor remains the sole owner of the child and all pipes.
4. Acquire one transcode-video lease through the API and construct the worker-protocol
   `OperationRequest` without flattening its typed lease ID. Preserve the acquired operation but
   replace only the dispatch payload with `{"mode":"crash","path":"/stress/process-crash"}`.
   The durable ticket payload is unchanged and remains the input to the synthetic retry. Extend
   `chaos-worker::dispatch_operation` to accept `TranscodeVideo` only when this parsed mode is
   `Crash`; its baseline and other fault operation allowlist remains unchanged.
5. Dispatch over the real worker socket under one five-second post-dispatch deadline covering both
   socket termination and the supervisor's child-exit observation. Accept only a connection
   termination paired with the child exiting non-zero. An ordinary terminal response, no lease,
   readiness timeout, early child exit, or post-dispatch timeout returns a phase-specific error to
   the unconditional shutdown path. Shutdown may use a bounded pre-kill grace, but its final wait
   continues until reap as ADR 0095 requires.
6. Await the child, remove it from the supervisor registry, and append one abandoned
   `ExecutionRecord` plus one `ProcessCrashObservation` containing PID, node ID, worker ID, ticket
   ID, lease ID, and exit status.

The runner never calls complete, fail, or lease heartbeat after dispatch. The lease remains held
until the existing control-plane recovery path expires it.

### Child supervisor

The integration harness starts one crate-private dedicated supervisor actor for the whole prelude. For each
spawn, that actor starts one child-watcher task and transfers the `tokio::process::Child`, stdin,
and stdout to it. The watcher is the sole child owner from spawn through wait; the actor owns its
shutdown sender and join handle in a registry. The runner communicates with the actor over a
bounded command channel using `Spawn`, `Wait`, and `ShutdownAll`. Replies are
`Ready { child_id, pid, bound }`, `Exited { child_id, status }`, or an operation-specific error.

Each watcher reads at most one 4 KiB newline-terminated readiness frame within five seconds, then
selects concurrently between `child.wait()` and its shutdown receiver. Natural exit returns the
status exactly once. When shutdown wins, the watcher closes stdin, allows the five-second grace,
issues kill if still running, and retains the sole child handle through the final wait. A pending
`Wait` reply receives the same exit status or cleanup error when shutdown wins; it is never left
waiting on a second owner. The actor itself continuously selects between runner commands and
watcher joins, so a stay-alive child cannot prevent it receiving `ShutdownAll` or noticing command
channel closure. It removes a registry entry only after its watcher join proves reap. A successful
watcher join stores `Exited { status }` as a completed-status tombstone when no `Wait` reply is
registered yet; the actor retains that tombstone until exactly one later `Wait` consumes the
status. When `Wait` arrives first, it registers one reply sender that the watcher join completes.
A second `Wait` for either a pending or consumed identity is rejected. The child identity leaves
the registry only after both reap and exactly-once status delivery, so a fast crash cannot erase
the observation before the runner subscribes while the command channel remains open.

Terminal supervisor shutdown is a separate transition. After command-channel closure, the actor
joins every watcher and retains reap/accounting for the shutdown result, completes every already
registered `Wait` reply, then discards completed-status tombstones for which no future `Wait` can
arrive. Exactly-once status delivery therefore governs live command service; terminal shutdown
governs resource cleanup and never waits for an impossible subscriber. Only after that sweep may
the actor assert an empty registry and finish. A successful crash is explicitly waited and
removed. Any
error or command-channel closure runs `shutdown_all`: close retained stdin, wait up to five
seconds, issue kill for a still-running child, then retain ownership and wait until that controlled
child is reaped. The final wait is intentionally not abandoned behind a second timeout: returning
without the wait would contradict the issue's child-reaping criterion. Cleanup
accumulates child-specific errors and still attempts every child. A non-cancellable outer test
owner runs the harness body in an inner task, then always requests shutdown and joins the
supervisor before returning, propagating a body panic/cancellation only after the registry is
proven empty. After either normal completion or cleanup, the registry must be empty.

The supervisor is test-only and accepts the binary path explicitly; it never searches `PATH`.
All bind addresses are literal loopback with port zero. The watcher owns and drains stdout's single
readiness frame under the byte and time limits above; `chaos-worker` emits no later stdout
contract. Stderr is inherited and stdin stays with the watcher so shutdown can close it before
waiting. Aborting the runner while it waits for `Ready` or `Exited` drops only a reply receiver;
command-channel closure still drives the actor's cleanup path, whose watcher retains the unique
child handle without a shared lock.

### Stress orchestration

Seed the workload before starting synthetic sessions. The selected process tickets are independent
roots placed first in deterministic seed order and retain the ordinary transcode media payload; dependency
generation begins after that prefix and never points a selected ticket at a prerequisite. Keep
every non-selected ticket pending while the prelude runs, then call the existing readiness
transition for the remainder only after every selected lease has been acquired and its child
reaped. Each prelude acquisition must name a member of the selected set that has not already been
observed; any mismatch stops dispatch and cleans up. This isolation, rather than seed order or
priority, guarantees the whole configured crash count is initially leasable even at the maximum
dependency percentage. Start and reap
their dedicated workers sequentially, recording the acquired leases. Then create the ordinary
synthetic sessions and enter the existing drain loop. Its first recovery wave snapshots the
process-crashed leases, advances the injected domain clock past their actual deadlines, and
requires the recovery report's expired set to match them. Synthetic workers acquire each retry and
settle it. Process observations are converted to abandoned execution records before the unchanged
conservation check runs.

## Failure and cleanup contract

- Invalid configuration fails before database, server, or process creation.
- Missing or non-executable binary, malformed readiness, readiness timeout, early clean exit,
  acquisition without a lease, unexpected worker terminal output, post-dispatch timeout, or an
  unreaped child returns an error naming the node/process phase.
- Once a child has spawned, ordinary error, inner-task panic, and inner-task cancellation paths all
  invoke and join supervisor cleanup before the outer test returns. Server and synthetic-session
  tasks retain the existing stop/join/abort ordering.
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
3. Test cleanup with a deliberately returned post-spawn error, panic, and cancelled inner body;
   prove in every case that the outer owner joins the supervisor, its registry is empty, and the
   child wait completed before the outcome propagates.
4. Run a small real-process cell and assert process count, non-zero exits, recorded lease identity,
   exact expiry, reassignment, terminal settlement, and zero supervised children.
5. Prove an identical durable transcode payload causes child exit 101 through the harness-owned
   crash dispatch override on attempt one and is accepted by the synthetic dispatcher on retry.
6. Seed higher-priority non-selected tickets and prove they remain pending until the selected crash
   set is acquired; inject a mismatched acquired ticket and require cleanup before dispatch.
7. Abort the runner while it awaits readiness and while it awaits exit; in both cases require the
   supervisor to retain ownership, reap the child, and report an empty registry.
8. Use a test child that emits more than 4 KiB without a newline and one that accepts dispatch but
   stays alive; require malformed-readiness and post-dispatch-timeout errors respectively, followed
   by kill, final wait, completion of the pending `Wait` reply, a joined actor, and an empty
   supervisor registry rather than merely timing out the outer test.
9. Delay `Wait` until after the watcher join and prove the completed-status tombstone returns the
   exact non-zero exit once; require a second `Wait` to fail and the registry to become empty only
   after the first status is consumed.
10. Let a child exit and its watcher join, then cancel the runner and close the command channel
    without sending `Wait`; require terminal shutdown to retain reap/accounting, discard the now
    undeliverable tombstone, join the actor, and report an empty registry.
11. Inject a duplicate terminal observation into the reused conservation input, observe the
   existing duplicate diagnostic, revert, and rerun green.
12. Run focused `voom-fakes` tests, `just stress` with the process arm, then `just ci`. Record the
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
