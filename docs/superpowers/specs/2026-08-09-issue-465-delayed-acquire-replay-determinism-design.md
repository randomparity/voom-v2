# Delayed Acquire Replay Determinism Design

Issue: #465

## Scope authority

- Scope identity: issue #465 plus token `1af46e42-ea01-437b-a323-00a34e6f2a06`.
- Outcome: make delayed-acquire replay failure diagnosis deterministic without claiming the
  unreproduced production timing flake is fixed.
- Completion criteria: replace anonymous counter polling with persistent named milestone signals;
  race every milestone against agent completion and the unchanged hang guard; make agent exit win
  simultaneous readiness; mutation-bite removal of each notification and ignored early exit;
  retain durable ticket and zero dispatch assertions; pass focused, loaded, and repository
  guardrails; deliver a green, mergeable PR.
- Provenance: issue #465's explicit deterministic-transition-or-diagnostic acceptance clause, plus
  the campaign orchestrator's 2026-08-09 scope decision after 256/256 instrumented loaded runs did
  not reproduce the underlying rare scheduler path.
- Exclusions: no claim that the production timing flake itself is fixed; no production runtime,
  API, schema, migration, timeout increase, or unrelated behavior change; the campaign manifest
  remains orchestrator-owned.
- Surface: `crates/voom-node-agent/tests/lifecycle.rs` and directly related design and plan artifacts.
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
That turns a concrete early-exit result into an ambiguous 30-second timeout. One focused run, one
16-way focused run, 64 loaded full-lifecycle-suite runs, and 256 instrumented loaded suites on the
current base all passed. These runs do not establish whether the original missing transition was
an early exit or a live-agent stall, and this design does not claim that underlying cause. They do
establish that reproduction is not currently a usable discovery mechanism.

The verified harness cause is narrower: the original failure cannot identify the absent transition
or distinguish a live stall from a completed agent because both waits share one line and discard the
join result. Issue #465 explicitly accepts reporting the stalled transition without a timing-sensitive
poll as the alternative when deterministic reproduction is unavailable. This change is diagnostic
instrumentation that removes that ambiguity; it does not declare the underlying runtime innocent.
If the instrumented test later reports a live-agent stall or early exit, that result is new causal
evidence for a linked production issue rather than authority to speculate in this change.

## Approaches

### Selected: named persistent transition notifications raced against agent exit

Replace the two progress counters with distinct `tokio::sync::Notify` values named for the
transitions the test requires: the first acquire response committed, and the replay acquire
started. `notify_one` stores a permit when the test has not begun waiting yet, so each notification
is persistent across the producer/consumer scheduling race. A helper races the named notification,
the mutable agent join handle, and `HANG_GUARD`.

The select is biased with the agent join arm first. Any agent completion before the test sends its
stop request is a failure, even if a transition notification is also ready, so the exit result cannot
be hidden by simultaneous readiness. Otherwise the transition wins immediately when reached. A
genuine stalled live agent still expires under the unchanged guard and identifies the exact missing
transition. Completion/failure counters remain because they are final negative assertions, not
synchronization.

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
handle, and a static transition description. Its biased `tokio::select!` has the agent completion
arm first, followed by notification and the guard sleep. The agent and timeout arms panic with
distinct actionable messages. A focused helper regression makes both agent completion and the
notification ready before polling and proves the exit arm wins. The test keeps the join handle
after both waits and performs the existing graceful-stop join and durable ticket assertions
unchanged.

This is a test-local synchronization choice, not a production ownership or public concurrency
contract, so it does not warrant an ADR. No production file changes.

## Testing and bite proof

The lifecycle test itself crosses the real HTTP middleware and runtime boundary, so it remains the
regression test. TDD starts by changing it to require the named notification fields and helper; the
focused build must fail because that synchronization surface does not yet exist. After the minimal
helper and middleware signals make it green, three mutation bites run independently:

1. remove the first-response-committed signal and temporarily change only the test-local
   `HANG_GUARD` to 500 milliseconds; the focused test must exit 101 with
   `live agent never reached first acquire response committed`;
2. restore the first signal, remove the replay-start signal under the same temporary guard, and
   require exit 101 with `live agent never reached replay acquire started`; and
3. restore both signals, remove biased agent-exit precedence, and run the simultaneous-readiness
   helper regression; it must fail because notification incorrectly wins over the completed agent.

Restoring every mutation must return both focused tests to exit 0. The temporary short guard
distinguishes the intended missing signal from setup failure and leaves the committed 30-second
guard unchanged.

The focused command is
`cargo test -p voom-node-agent --test lifecycle delayed_acquire_replay_never_dispatches -- --exact`.
The simultaneous-readiness helper regression runs by its exact test name. The concurrency check runs
four waves of 16 direct lifecycle test-binary copies, for 64 total suites, and requires every process
to exit 0; this matches the controlled reproduction command recorded in the implementation plan.
The completed baseline was 64/64 before the change. The same 64/64 threshold applies after it.
`just ci` is the final repository guardrail.

## Success criteria

- The two acquire transitions use distinct persistent notifications rather than counter polling.
- Each wait terminates on transition, early agent exit, or the unchanged deadlock guard.
- Agent exit wins when exit and transition are simultaneously ready; the failure names the missing
  transition and includes the exit result.
- Ticket state and zero complete/fail dispatch assertions remain unchanged.
- No production behavior, public contract, dependency, timeout, or migration changes.
- Focused, concurrent, bite, and repository guardrails pass without unexpected skips or warnings.
