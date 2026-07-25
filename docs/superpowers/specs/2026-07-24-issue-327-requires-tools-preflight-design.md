---
name: requires-tools-preflight-design
description: Typed policy-tool validation and pre-execution capability enforcement for metadata.requires_tools.
status: accepted
date: 2026-07-24
issue: 327
references:
  - docs/adr/0002-out-of-process-workers-only.md
  - docs/adr/0013-payload-evolution-contract.md
  - docs/adr/0034-policy-tool-requirements-use-worker-capabilities.md
  - docs/specs/voom-control-plane-design.md
---

# Policy Tool Requirement Preflight (#327)

## Goal

Make the published `metadata.requires_tools` declaration executable. Known V1
tool names compile without a deferred warning, and every policy execution path
checks the declared tools before opening a job. An initially unavailable tool
fails with a complete actionable diagnostic and leaves issue and execution state
unopened.

## Scope

In scope:

- the published tool tokens `ffmpeg`, `ffprobe`, and `mkvtoolnix`;
- typed validation and access over the existing compiled metadata map;
- compatibility with stored schema-version-2 compiled policies;
- supervisor-owned identities for run-local ffmpeg/mkvtoolnix providers;
- fresh bundled ffprobe dependency readiness and recoverable built-in
  incarnations;
- one prepared-input preflight shared by compliance execute, direct coordinator
  execution, and resume;
- focused tests and the executable published-grammar golden.

Out of scope:

- new metadata tool names or parser syntax;
- remote provider identity;
- worker installation or auto-start for mutation providers;
- atomic worker eligibility at lease acquisition (#343);
- remux and verify-artifact execution wiring owned by #331, #332, and #336.

## Compiled contract

Add a public closed `PolicyTool` enum in `voom-policy` with exactly three
snake-case names and a typed accessor:

```rust
pub fn required_tools(&self) -> Result<Vec<PolicyTool>, RequiredToolsError>
```

No serialized `CompiledPolicy` field is added. The existing
`metadata.requires_tools` array remains the one durable representation, so
compiled JSON shape, schema version, source hash, and rollback behavior do not
change.

For newly compiled source, validation requires:

- the value is a list;
- every entry is an unquoted identifier;
- every identifier is one of `ffmpeg`, `ffprobe`, or `mkvtoolnix`;
- duplicate entries are returned once in source order.

The `requires_tools` metadata key may appear only once. Repeated settings are a
validation error, even if both lists are individually valid, so metadata-map
lowering cannot silently replace an earlier requirement.

Valid declarations emit no warning. Scalar, quoted, or unknown entries are
validation errors. This changes no parser production: the parser continues to
parse generic metadata, and semantic validation narrows the published setting.

For existing compiled JSON, lexical provenance is gone because both a quoted
string and an identifier were lowered to a JSON string. The typed compatibility
accessor accepts canonical JSON strings regardless of their erased source
spelling. It rejects a non-array, non-string entry, or unknown tool. This is a
stored-data compatibility rule, not a newly accepted source form.

When the control plane successfully types a stored requirement, it removes the
obsolete `metadata_requires_tools_deferred` warning in memory. Stored JSON is
not rewritten.

## Provider identity

Literal tool requirements cannot be inferred from implementation-neutral
operation names. The control plane reserves these worker-name namespaces:

- `local-ffmpeg-<unique>`;
- `local-mkvtoolnix-<unique>`;
- `builtin.ffprobe` and `builtin.ffprobe-<unique>`.

General node-less and node-owned registration rejects reserved names and
prefixes. Internal supervisor/bootstrap paths create them with
`WorkerKind::Local` and `node_id: None`. The run-local supervisor chooses the
matching version-locked binary, generates the worker secret, and records
capabilities only after the child reports `BOUND`.

Worker protocol v2 adds a server-authenticating `POST /v1/identity` route. The
request carries the offered protocol version and a fresh 128-bit random
challenge, but not the bearer credential:

```rust
struct WorkerIdentityRequest {
    offered: u32,
    challenge: String,
}

struct WorkerIdentityResponse {
    worker_id: WorkerId,
    worker_epoch: u64,
    protocol_version: u32,
    proof: String,
}
```

The server derives a 32-byte BLAKE3 key from the worker secret using the context
`voom-worker-identity-v1`. It returns a keyed hash over a canonical binary
message containing the context, offered version, challenge bytes, worker ID,
worker epoch, and response protocol version. `HttpClient::identity` generates
each 128-bit challenge from the OS-seeded `rand` CSPRNG and verifies decoded
proof bytes with the existing constant-time equality helper before accepting
the returned identity. The response must also match the expected worker
ID/epoch and `PROTOCOL_VERSION`. An echo endpoint that does not possess the
secret therefore fails verification, and a proof captured for one challenge
cannot authenticate a later challenge.

This route is read-only: it invokes no operation handler and creates no
idempotency entry. The whole identity round-trip, including response-body
collection, has a ten-second deadline matching the current handshake deadline.
The existing unauthenticated `/v1/handshake` remains version negotiation, not
identity evidence. `PROTOCOL_VERSION` bumps from 1 to 2 under ADR 0016's
flag-day rule.

This makes a caller-supplied local/remote/synthetic worker with a matching
operation insufficient, even if it imitates the visible prefix through direct
input. Direct database modification remains outside the application trust
boundary.

## Capability model

The preflight receives the compiled policy and the liveness-filtered
`WorkerRuntimeRegistry`.

| Tool | Required evidence |
|---|---|
| `ffmpeg` | Registered/active reserved ffmpeg incarnation in the live registry, with one effective ffmpeg-family capability |
| `mkvtoolnix` | Registered/active reserved mkvtoolnix incarnation in the live registry, with effective `remux` capability |
| `ffprobe` | Fresh bundled ffprobe startup and handshake, then a registered/active reserved built-in incarnation with effective `probe_file` capability |

An effective operation requires:

- a matching `worker_capabilities` row;
- at least one matching `can_execute` grant;
- no matching deny across any grant row;
- worker status `registered` or `active`.

Mutation providers must also prove the recorded identity challenge. Wrong
identity, stale, retired, denied, ungranted, capability-only, dead-endpoint, or
credential-mismatched workers do not satisfy a requirement.

### Bundled ffprobe readiness and recovery

`voom-ffprobe-worker` changes startup so `ffprobe -version` must start, finish
within the existing version timeout, exit successfully, and yield a recognized
version before the HTTP server binds. Failure becomes
`WorkerStartupError::Dependency`; the process never prints `BOUND`.

The control-plane readiness probe:

1. launches the real bundled ffprobe worker under temporary credentials;
2. waits for `BOUND` and completes the identity challenge-response;
3. shuts down and reaps the process on every success, timeout, and error path;
4. in a transaction, resolves a live built-in incarnation and its effective
   `probe_file` capability.

The bootstrap resolver uses the repository's `BEGIN IMMEDIATE` transaction
pattern and re-reads all live reserved ffprobe rows after taking the write
reservation. It adopts the sole live row, including the legacy
`builtin.ffprobe` name. If none exists because rows are absent, stale, or
retired, it registers one new node-less local row under the reserved unique
prefix and records capability and grant. A sole live denied incarnation fails
with its deny context rather than being replaced. Multiple live reserved
incarnations are a durable invariant error with operator context; the resolver
does not choose among them. General registration cannot claim any built-in
prefix.

This order avoids leaving a fresh capability row when the executable or bundled
worker is unavailable. Concurrent preflights serialize at the resolver and
converge on the same live row without a spurious SQLite busy failure. A later
loss can still fail a real operation; preflight is not a lease.

## Execution ordering

Compliance execute performs:

1. discover endpoint runtimes and probe liveness;
2. generate the read-only compliance report;
3. load policy/input, type requirements, preflight tools, and retain prepared
   phase-barrier inputs;
4. enforce the optional safety policy and existing plan endpoint checks;
5. apply report findings;
6. open the coordinator job from the prepared inputs and dispatch.

Direct coordinator and resume callers build the same prepared inputs before
`with_phase_barrier_job`. Compliance execute passes prepared inputs to an
internal coordinator entry point, so it does not repeat preflight after applying
findings.

The guarantee is precise and observational: during the preparation interval,
each declared tool independently produced the evidence above, and any
requirement whose observation fails produces no issue or execution mutation.
The checks are sequential, so the design does not claim all tools were
simultaneously available or remain available when preparation returns. A later
loss is an execution failure with the normal partial outcome; it is not falsely
reported as a successful run. Reserving capabilities or making lease
acquisition atomic is outside #327.

The check is readiness evidence, not dispatch authorization. Today the
scheduler evaluates grant rows independently. A split allow/deny state, or a
deny introduced after preparation, can therefore still permit dispatch despite
that deny. Issue #343 owns the stable effective-eligibility predicate and its
atomic enforcement at lease acquisition. #327 does not claim scheduler deny
semantics are trustworthy before that issue is resolved.

## Failure behavior

After metadata typing succeeds, each tool check returns a typed observation:
`Available` or `Unavailable { reason, guidance }`. The unavailable category
includes missing, stale, retired, ungranted, denied, dead-endpoint,
wrong-identity, credential-mismatch, identity-timeout, and bundled dependency
failure. All unavailable observations are collected before returning, so mixed
failures produce one deterministic source-ordered
`VoomError::PolicyExecution` and the public `POLICY_EXECUTION_ERROR` code, with:

- the policy slug;
- every unavailable tool and its specific reason;
- per-tool `voom worker run-local --kind ffmpeg` or `mkvtoolnix` guidance;
- bundled ffprobe startup/identity context when ffprobe readiness fails.

Malformed legacy metadata returns a policy execution error naming the invalid
value before provider observation begins. A database failure or durable
reserved-identity invariant violation aborts observation immediately with its
specific error context because the system cannot reliably complete the
inspection; the complete-list promise applies only to typed tool-unavailability
observations. No job/ticket event is created for any initial failure.

## Compatibility and rollback

- There is no migration or new durable JSON field.
- Old and new binaries read exactly the same compiled metadata shape.
- Valid old declarations gain execution semantics through the typed accessor.
- Old unknown or malformed declarations remain readable as JSON but fail loud
  when execution tries to use them.
- Stored deferred warnings are normalized only in memory.
- #327 adds no compiled JSON field and therefore introduces no new payload
  shape. The pre-existing mismatch between the Class-P inventory entry and the
  higher-layer typed `CompiledPolicy` read is tracked by #344.

## Verify worker boundary

`builtin.verify_artifact` remains a real bundled capability, but it maps to no
published `PolicyTool`. This design does not invent `verify` or interpret
`verify artifact` as a metadata tool declaration.

## Test strategy

`voom-policy`:

- all three published identifiers type in source order with duplicates removed;
- repeated `requires_tools` metadata settings fail validation rather than
  replacing an earlier list;
- valid identifiers produce no deferred warning;
- scalar, quoted, and unknown source forms fail validation;
- legacy canonical JSON strings type successfully;
- malformed/unknown legacy JSON fails loud;
- deterministic compiled JSON has no new field.

`voom-ffprobe-worker`:

- missing executable, non-zero exit, timeout, and malformed version prevent
  startup readiness;
- a valid executable reaches worker readiness.

`voom-control-plane`:

- general registration rejects every reserved provider namespace;
- supervisor-created live/granted ffmpeg and mkvtoolnix workers pass;
- a protocol-compatible endpoint with the wrong secret or worker ID/epoch
  fails the identity challenge;
- an endpoint that echoes the challenge and claimed identity without the secret
  fails proof verification;
- two identity calls carry distinct challenges, and replaying the first valid
  proof against the second challenge fails;
- a bound endpoint that never answers the identity route times out without
  opening execution state;
- matching-operation wrong-identity, missing, stale/retired, ungranted, denied,
  and dead-endpoint providers fail;
- separate allow and deny grant rows for the same operation make that operation
  unavailable during preflight;
- multiple missing tools appear together deterministically;
- missing/stale/retired built-in ffprobe rows create a fresh incarnation only
  after process readiness;
- a denied live ffprobe incarnation fails without replacement;
- concurrent ffprobe preflights converge on one live incarnation without a busy
  error, while pre-existing multiple live rows fail as an invariant violation;
- every ffprobe readiness failure and timeout reaps its temporary child;
- mixed missing, denied, dead-endpoint, and dependency failures are reported in
  source order with per-tool reasons;
- initial preflight failure creates no compliance issue or job;
- direct execution and resume both prepare before opening a job.

Corpus:

- the published-grammar policy compiles without the deferred warning;
- its deterministic compiled golden retains the existing metadata shape;
- coverage row S02 moves to executable control-plane coverage.

Verification commands:

- focused `cargo test -p voom-policy`;
- focused `cargo test -p voom-worker-protocol`;
- focused `cargo test -p voom-ffprobe-worker`;
- focused `cargo test -p voom-control-plane`;
- `just fmt-check`, `just lint`, and `just ci`.

## Success criteria

- Known published tools compile without a deferred warning.
- Existing compiled policy versions remain readable and enforce valid
  declarations.
- Every execution entry point checks concrete, live capabilities before opening
  a job.
- Initial missing capability failures report all missing tools and leave issue
  and execution state untouched.
- No parser-only or unpublished DSL form is accepted.
