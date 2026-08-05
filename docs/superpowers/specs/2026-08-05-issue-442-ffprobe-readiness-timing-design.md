# Issue #442: Deterministic ffprobe readiness timing

## Scope authority

This design implements the frozen `WORK:SCOPE` charter on issue #442, token
`scope-442-7ddac43b`. The issue body and campaign dispatch are the external authority. The
change must make ffprobe readiness tolerant of ordinary scheduler load, keep hung-child
termination and reaping actionable, and make the deny-plus-missing policy aggregation test
independent of host media executables.

The permitted surface is the ffprobe-worker version probe and tests, the control-plane
policy-preflight ffprobe observation boundary and tests, and directly required design
documentation. Failure-order redesign and process-global environment mutation are excluded.
There are no unresolved design-changing ambiguities.

## Root cause

`FfprobeConfig` starts `ffprobe -version` synchronously during worker startup and rejects it
after one wall-clock second. The bundled-worker supervisor separately waits five seconds for
the worker's bound-address line. Under parallel hosted-CI scheduling, a responsive ffprobe
can cross the inner deadline, so the worker emits a dependency failure even though the
outer startup budget has not expired.

`ControlPlane::preflight_policy_tools` directly calls the real bundled readiness probe when
it reaches an `ffprobe` requirement. Consequently,
`denied_ffprobe_is_aggregated_with_later_missing_tool` starts a real worker and host
ffprobe before it can inspect the deliberately denied durable provider. A host-timing fault
there replaces the deny diagnostic the test intends to aggregate.

No matching solution record exists. The related ffmpeg pipe-deadlock solution concerns
draining piped output, not scheduler tolerance; ffprobe already collects output, kills the
timed-out child, and waits for it.

## Approaches

### Selected: inject only readiness and enlarge the inner budget

Keep the production sequence intact and split a private policy-preflight helper at the
bundled-readiness call. The public crate-internal entry point supplies the real async probe;
the aggregation test supplies an immediately successful callback. Independently, make the
private ffprobe version detector accept a duration, with public constructors choosing four
seconds and the timeout test choosing a short duration.

This preserves ownership and failure order, tests the actual durable deny path, and creates
no public or operator-facing surface. ADR 0056 records the timing and injection decision.

### Rejected: environment-installed fake executable

Pointing `VOOM_FFPROBE_BIN` or the bundled-worker path at a helper would still start a child
for a database-aggregation test and would mutate process-global environment. Serializing
that mutation hides the architectural boundary instead of testing it.

### Rejected: eligibility-first short circuit

Reading the persisted deny before fresh readiness would avoid the host executable, but it
would redesign failure ordering. ADR 0034 makes real bundled readiness precede built-in row
resolution so a durable row never masquerades as current dependency evidence.

### Rejected: shared public deadline contract

Moving both timeout constants to a shared crate would create a public timing surface for two
private process owners. The fixed five-second supervisor budget already supplies the bound;
four seconds is deliberately below it. A new shared abstraction is unnecessary for this
focused correction.

## Design

### FFprobe version boundary

`FfprobeConfig::from_process_env` and `FfprobeConfig::from_env_pairs` retain their signatures
and behavior except for selecting a four-second production version deadline. They converge
on a private constructor that accepts the resolved binary and a version-probe duration.
`detect_ffprobe_version` accepts that duration instead of consulting a hard-coded value.

The timeout loop remains synchronous because configuration is constructed before the Tokio
HTTP server. On expiry it kills the child, waits for it, and returns
`FfprobeConfigError::Timeout` containing the binary and the injected duration. Spawn,
polling, collection, non-zero exit, and malformed-version classifications do not change.

Four seconds fits strictly inside the control plane's existing five-second bundled-worker
startup timeout. The remaining second is coordination allowance for propagating a
dependency error or binding the loopback server and printing readiness. This is not a claim
that any ffprobe completing within four seconds guarantees total startup within five; the
outer bound remains authoritative.

### Policy-preflight boundary

`preflight_policy_tools` remains the production entry point. It delegates to a private
generic helper that accepts an async zero-argument readiness callback. Production passes
`verify_bundled_ffprobe_readiness`; tests can pass a hermetic callback returning success.

The helper still normalizes requirements first, visits them in metadata order, and invokes
the callback only in the `PolicyTool::Ffprobe` arm. The readiness result is passed into the
existing built-in observation step, which then reads/repairs eligibility exactly as today.
The denied-row test therefore exercises the real transaction and deny classification while
substituting only the unrelated host process.

No environment variable, durable schema, public API, or CLI output changes. Readiness still
precedes built-in eligibility, and all unavailable observations are still aggregated in
metadata order.

## Failure behavior

- A missing ffprobe still reports the `start version check` I/O error.
- A hung version process is killed and reaped at the injected deadline, then reports the
  executable and duration as a dependency timeout.
- Kill or reap failure remains an I/O error naming the failed operation.
- A non-zero or malformed version response retains its existing classification.
- A production bundled-readiness failure remains an unavailable ffprobe observation.
- The hermetic test callback cannot affect production because it is accepted only by a
  private helper; the production entry point always supplies the real probe.

## Testing

1. Change the policy aggregation test first to call the not-yet-existing injected helper,
   return readiness success in memory, count exactly one invocation, and retain assertions
   for both findings and their metadata order. The compile failure is the red proof.
2. Change the ffprobe hung-child test first to request a short private deadline, have the
   helper record its PID before hanging, and assert the returned timeout duration,
   actionable text, and that `kill -0` cannot find the reaped PID. The missing private
   boundary is the red proof.
3. Implement the two private boundaries and four-second production value. Run the focused
   ffprobe-worker library suite and control-plane policy-preflight suite.
4. Run focused Clippy for both crates. After review and campaign heavy-gate clearance, run
   fresh bare `just ci` and `just smoke`.

The tests remain Unix-only where they already depend on executable shell helpers. They use
temporary fixtures and no process-global environment.

## Success criteria

- Denied ffprobe and missing ffmpeg produce both diagnostics in metadata order without
  starting a host ffprobe.
- Public ffprobe configuration uses a four-second version deadline below the five-second
  worker-startup bound.
- A short injected deadline deterministically proves timeout, kill, reap, and actionable
  error details for a hung child.
- Readiness-before-eligibility ordering, all existing error classifications, and public
  constructors remain unchanged.
- Focused suites, focused Clippy, fresh `just ci`, and `just smoke` pass with no skipped
  required checks.
