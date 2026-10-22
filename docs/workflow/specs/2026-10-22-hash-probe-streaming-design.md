# Hash/probe streaming dispatch (#723)

## Authority and problem

Scope: #723, token `q723-c43d488e`; operator-approved prerequisite to #339.
Valid hash/probe requests currently await whole-file work before their first response.
Measured large-file first responses exceeded the unchanged 30-second dispatch deadline.

## Design

Keep ownership in the two existing workers. Parse operation and payload synchronously,
then create `OperationDispatch::streaming` before polling the file-work future.
Write initial progress sequence 0, spawn one task per dispatch, and return the response.
In that task, select between the pinned operation future and a progress timer using
ffmpeg's existing interval formula: half the requested idle deadline, clamped to
1–5000 milliseconds. The timer describes work as active, not bytes completed.
Keep the operation future alive across ticks. Prefer a ready result over a timer tick.
Use one increasing sequence counter for progress and exactly one terminal result/error.
Writer failure ends the task and drops the unfinished operation; terminal writes use
the existing writer's completion/cache behavior. Do not add a shared streaming abstraction.

Wrong operation remains a protocol error. Malformed payload remains a replayable buffered
HTTP 200 error. Valid operations, including operational failures, stream through the
existing frames. Serialization failure becomes the existing malformed-result error.
Preserve payload schemas, hash/root descent, pre/post facts, ffprobe subprocess behavior,
checksum validation, error taxonomy, idempotency and replay behavior. Existing worker
request deadlines and HTTP authentication are unchanged. No caller migration is needed.

## Alternatives and existing decision

Reuse the ffmpeg streaming pattern (chosen): matches the installed protocol mechanism.
Increasing the global dispatch deadline would keep acknowledgement coupled to file size.
Moving work into the control plane conflicts with byte ownership in ADR 0050/0077.
This applies an existing lifecycle; no new architecture or protocol decision is introduced.

## Success and verification

For these two workers, deterministic real-HTTP tests hold work pending behind a barrier,
receive acknowledgement and multiple ordered progress frames before release, then observe
one terminal frame and completed replay without rerunning work. Exercise success and error.
Keep malformed-payload, wrong-operation, checksum, containment and drift regressions.
Fault-inject delayed acknowledgement or disabled progress to prove the new tests fail.
Use the retained large input with newly built binaries and normal dispatch deadlines,
then run the original eight-input rooted scan with the authenticated owner-agent flow.
Check successful session/ticket, released lease, consumed access plan and observations;
retain failed historical scan and evidence. This prerequisite does not start policy execution.

## Failure model

1. Deployment: authenticated storage-owner workers on supported native platforms; legitimate
   slow regular-file reads and ffprobe subprocesses; operator-selected retained canary.
2. Must prevent: first-response starvation, progress-idle starvation while asynchronous work
   is pending, duplicate terminal frames, replay reruns and altered integrity/error semantics.
3. Accepted: task/process crash may interrupt unfinished work under existing lease/retry rules;
   no guarantee against a completely stalled runtime or blocking kernel call. Progress is
   liveness, not a throughput or completion deadline, as in the existing ffmpeg contract.
4. Covered elsewhere: worker authentication/admission/replay cache by worker-protocol;
   rooted access and source selection by node-agent/#339; production/retention by operator.

## Threat model

- Boundaries: existing authenticated operation payload to worker; path to local filesystem;
  worker frames to authenticated caller and replay cache. No new or widened boundary.
- Actors: authenticated agent supplies paths/deadlines; media filesystem can change during reads.
- Controls: existing payload decoding, hash symlink-free rooted descent, probe no-follow regular
  file open, pre/post integrity checks; protocol owns auth/idempotency. Existing error payloads
  retain their diagnostic content. Timed frames contain fixed messages and no new media data.
- Out of scope: global stream/cache admission bounds and protocol lifecycle policy, unchanged
  here; unrelated path authorization changes, new credentials, production and source mutation.

## Global constraints

Rust 1.95.0 pinned; no new dependencies, DSL, schema, authorization or global-timeout changes.
Targets: x86_64-unknown-linux-gnu, aarch64-unknown-linux-gnu, aarch64-apple-darwin.
Preserve retained artifacts and source media. Keep tests in sibling files or integration tests.
