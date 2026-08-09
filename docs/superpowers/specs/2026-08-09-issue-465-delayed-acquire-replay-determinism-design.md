# Delayed Acquire Replay Determinism Design

Issue: #465

## Scope authority

- Scope identity: issue #465 plus token `b0ab5e02-be96-44d5-8c3f-358725a91c46`.
- Outcome: make the delayed-acquire replay lifecycle test deterministic.
- Completion criteria: reproduce or instrument the intermittent stall; identify its cause;
  synchronize on the transition under test without a timing-sensitive counter poll; retain the
  existing hang guard as a deadlock detector; prove the regression test bites; pass repository
  guardrails; deliver a green, mergeable PR.
- Provenance: public issue #465 and its linked discovery from issue #463.
- Exclusions: no increased timeout; no API, schema, migration, or unrelated production behavior
  change; the campaign manifest remains orchestrator-owned.
- Surface: `crates/voom-node-agent/tests/lifecycle.rs`; acquisition-loop code only if root-cause
  evidence requires it; directly related test helpers and this design and plan.
- Ambiguities: none.
- Interaction: unattended campaign subagent.

## Diagnosis

The failure is reported by the shared `wait_for_count` helper, so its line number cannot identify
whether the first response never committed or the replay acquire never started. Both counters are
monotonic `SeqCst` atomics. Once either transition occurs, the corresponding value remains visible;
therefore the 25-millisecond polling interval cannot lose a completed transition and is not itself
the cause of a permanent wait.

The harness does discard decisive state: it spawns the agent and does not observe the join handle
while waiting for either counter. If the runtime exits before a transition, the helper continues
polling an unreachable count until `HANG_GUARD` expires and reports only “counter never reached.”
That turns a concrete early-exit result into an ambiguous 30-second timeout. A 16-way controlled
run on the current base completed without reproducing the intermittent exit, consistent with the
original one-off failure and immediate passing rerun; nondeterministic reproduction is not treated
as evidence that the harness is sound.

## Approaches

### Selected: named persistent transition notifications raced against agent exit

Replace the two progress counters with distinct `tokio::sync::Notify` values named for the
transitions the test requires: the first acquire response committed, and the replay acquire
started. `notify_one` stores a permit when the test has not begun waiting yet, so each notification
is persistent across the producer/consumer scheduling race. A helper races the named notification,
the mutable agent join handle, and `HANG_GUARD`.

The transition wins immediately when reached. An early agent exit fails immediately with the join
result and transition name. A genuine stalled live agent still expires under the unchanged guard
and identifies the exact missing transition. Completion/failure counters remain because they are
final negative assertions, not synchronization.

### Rejected: keep atomic polling and improve only the error strings

Separate strings would identify the missing counter but would still delay a known early exit for
30 seconds and would retain periodic sampling where the middleware can signal the event directly.

### Rejected: increase the timeout or poll faster

The guard is a deadlock detector, and prior lifecycle investigation found bimodal completion versus
full-budget hangs. More time or more frequent polling neither observes agent exit nor makes an
unreachable transition occur.

### Rejected: change the production acquisition coordinator

The monotonic counters show no lost production transition, the exact rerun and 16-way controlled
run pass, and the current harness suppresses the agent result needed to establish a production
cause. Changing runtime behavior would be speculative. The improved failure preserves that result
for any future causal investigation.

## Detailed design

`DelayedAcquire` owns three notifications. The existing `release_response` notification continues
to gate the first response. After the real acquire handler returns, middleware signals
`first_response_committed` and then awaits release. On every later acquire attempt, middleware
signals `replay_acquire_started` before returning the synthetic idle response.

`wait_for_acquire_transition` accepts the notification, a mutable reference to the agent join
handle, and a static transition description. Its `tokio::select!` has three arms: notification,
agent completion, and the guard sleep. The agent and timeout arms panic with distinct actionable
messages. The test keeps the join handle after both waits and performs the existing graceful-stop
join and durable ticket assertions unchanged.

This is a test-local synchronization choice, not a production ownership or public concurrency
contract, so it does not warrant an ADR. No production file changes.

## Testing and bite proof

The lifecycle test itself crosses the real HTTP middleware and runtime boundary, so it remains the
regression test. TDD starts by changing it to require the named notification fields and helper; the
focused build must fail because that synchronization surface does not yet exist. After the minimal
helper and middleware signals make it green, bite proof temporarily removes the replay-start signal
and runs the focused test under a short external deadline. The test must remain blocked waiting for
that named transition; restoring the signal must return it to green. A concurrent focused run then
checks that notification permits survive producer/consumer scheduling variation.

`cargo test -p voom-node-agent --test lifecycle delayed_acquire_replay_never_dispatches -- --exact`
is the focused check. `just ci` is the final repository guardrail.

## Success criteria

- The two acquire transitions use distinct persistent notifications rather than counter polling.
- Each wait terminates on transition, early agent exit, or the unchanged deadlock guard.
- A failure names the missing transition and includes an early agent result when available.
- Ticket state and zero complete/fail dispatch assertions remain unchanged.
- No production behavior, public contract, dependency, timeout, or migration changes.
- Focused, concurrent, bite, and repository guardrails pass without unexpected skips or warnings.
