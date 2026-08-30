# ADR 0095: Preseed stress recovery with process-crashed attempts

## Status

Accepted

## Context

The distributed stress harness from ADR 0094 proves lease expiry and retry by abandoning an
in-process runner lane. Issue #606 requires the same conservation proof after operating-system
worker subprocesses terminate over real sockets, including evidence that every child is reaped.

The existing `chaos-worker` exits from its request handler in crash mode. A retry of the same
durable ticket carries the same payload, so sending both attempts to chaos workers would crash
both and exhaust the ticket. Activating replacement workers repeatedly on one remote node would
also replace its incarnation and fence earlier workers.

## Decision

Add a process-crash prelude to the opt-in stress harness. Before the ordinary synthetic node
sessions start, create one registered remote node per configured crash attempt. Seed every ticket
selected for this prelude as an independent ready root, regardless of the configured dependency
mix; generate dependencies only among the remaining workload, and withhold every non-selected
ticket from readiness until the prelude has acquired the complete selected set. For each crash node,
activate one worker, launch one `chaos-worker` subprocess using that activated worker identity,
acquire one transcode-video lease, and dispatch it over the worker protocol with a harness-owned
crash payload while preserving the acquired lease ID and operation. The durable ticket payload is
not mutated; the synthetic retry receives that original media payload. Extend `chaos-worker` only
enough to accept `TranscodeVideo` in crash mode, before baseline payload decoding. Require the child to
exit non-zero only after dispatch has begun, await and record its exit, and leave the acquired
lease unsettled. Retain a typed observation containing the child PID, node, worker, ticket, lease,
and exit status.

After all selected children are reaped, run the existing recovery coordinator and synthetic node
sessions. Recovery expires exactly the process-crashed leases; their second attempts use the
existing in-process synthetic dispatcher and can settle normally. Merge the process observations
into the existing execution log as abandoned first attempts, then reuse the existing conservation
assertion unchanged.

Own every child in a dedicated supervisor task rather than in the cancellable harness future. The
supervisor retains every child handle, closes stdin, waits for normal exit, escalates to kill on a
bounded timeout, and then owns the final wait until the controlled child is reaped. Dropping the harness-side command sender is itself a shutdown
signal. A non-cancellable outer test owner always joins the supervisor task after the harness body
returns, panics, or is cancelled; it does not report the test outcome until the join proves an
empty registry. Each child also enables Tokio `kill_on_drop` only as process-termination damage
containment if the whole test runtime is destroyed; it is not reaping evidence.

After the first child is registered, make cleanup unconditional for every ordinary `Result`
return: retain the prelude result, always request shutdown/reap, join the supervisor, verify its
registry is empty, and only then return the original result or a combined error that preserves
both the operation and cleanup failures. Panic and cancellation tests run the harness body in a
spawned inner task; the outer test owner observes the join failure or abort, then performs the same
shutdown-and-join sequence before propagating it.

The number of process crashes is derived from a validated opt-in percentage and ticket count,
rounded down with a minimum of one when the percentage is non-zero. It may not exceed the number
of tickets or consume all allowed attempts. Those selected tickets are the guaranteed initially
ready frontier, so every valid configured count is leasable before synthetic sessions start. The
prelude also verifies every acquired ticket belongs to that selected set before dispatch. The
default remains zero so `just stress` keeps its current cost and behavior.

## Consequences

- Every selected first attempt crosses the API socket and worker-protocol socket before a real
  child exits.
- Recovery and conservation remain owned by the existing harness rather than a second test oracle.
- Each crash node has its own incarnation, so later activations cannot fence another crashed
  lease accidentally.
- Retry execution is synthetic; the test covers process death, socket teardown, lease expiry,
  reassignment, and settlement, but not a healthy replacement subprocess.
- The opt-in cell pays one process launch per configured crash and reports process identities and
  lifecycle observations for reproduction.
- `kill_on_drop` limits damage during panic, while explicit bounded wait/kill/wait proves reaping
  on ordinary success and returned-error paths.
- A failure injected after one child starts must observe that child's completed wait before the
  harness returns, proving the unconditional cleanup structure bites.
- Panic and cancellation after child registration must be caught by the outer test owner, which
  joins the supervisor and proves the registry empty before propagating the test failure.

## Considered & rejected

- **Send both attempts to `chaos-worker` in crash mode.** verified: `streaming_or_fault_response`
  in `crates/voom-fakes/src/bin/chaos_worker.rs` selects `ExitProcess(101)` from the durable
  request payload, so an unchanged retry payload crashes again and cannot converge.
- **Mutate the durable ticket payload after the crash.** judgment: changing persisted work to make
  a test retry pass would test a path production recovery does not use and expands the issue into
  production storage behavior.
- **Use the media payload itself to select chaos mode.** verified: the stress media payload is
  decoded by the synthetic retry under deny-unknown-field contracts, while `chaos-worker` expects
  its own `mode`/`path` payload; a harness-owned dispatch override keeps both consumers valid.
- **Activate every crash worker on one node.** verified: ADR 0094 and
  `remote_execution/activation.rs` establish that activation replaces a node incarnation and
  fences its prior workers; distinct nodes preserve the intended crashed leases.
- **Replace the existing stress harness with a subprocess-only harness.** judgment: duplicating
  workload, recovery, and conservation logic adds surface without improving the process-death
  proof required by #606.
- **Trust `kill_on_drop` as cleanup evidence.** verified: Tokio documents `kill_on_drop` as a kill
  request when the handle is dropped; it does not await the child, so it cannot prove reaping.
- **Run the process-crash cell in routine CI.** judgment: issue #606 requires an opt-in path, while
  scheduled stress CI remains owned by #582.
