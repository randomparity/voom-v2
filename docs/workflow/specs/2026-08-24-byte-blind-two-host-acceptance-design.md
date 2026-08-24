# Byte-blind two-host acceptance — design spec

Issue #425 · branch `feat/byte-blind-acceptance-425` · base `main`

## Scope and authority

This acceptance proof implements issue #425 under accepted ADRs 0050, 0055, 0075,
0076, and 0077. It does not introduce a new production contract. In particular:

- ADR 0050 requires an operating-system denial, equal owner-scoped path strings,
  owner-local scheduling, no media transfer, scan reconciliation only after a
  complete traversal, and claims that remain live through post-dispatch work.
- ADR 0055 already makes provider locators unique within an owner rather than
  globally.
- ADR 0075 already puts handle-shaped media dispatch, local fact checks,
  verification, and add-only commit on the owner agent.
- ADR 0076 already makes tool readiness owner-scoped.
- ADR 0077 already routes scan, hash, and probe through the owner agent.

The deliverable is therefore a black-box acceptance environment, not another
architecture decision or an alternate orchestrator.

## Goals

1. Run a generated-media scan and policy workflow through real SQLite, the
   production control-plane cases and API router, two production node-agent
   processes, and the production scan/hash/probe/FFmpeg/MKVToolNix/verification
   workers.
2. Make the configured media, staging, and output paths unavailable in the
   control-plane mount namespace before the first byte-touching request, and
   assert the denial from that namespace.
3. Give both agents the same absolute provider-locator strings while binding
   those strings to different backing directories in private mount namespaces.
4. Exercise and prove every issue #425 acceptance criterion with durable-state,
   process, filesystem, and channel observations rather than source-text checks.
5. Keep the harness generated-only, deterministic, bounded, Linux-hermetic, and
   safe to run alongside the rest of the workspace.
6. Supply an operator runbook for a real remote storage host without making that
   host, its paths, or SSH availability a CI prerequisite.

## Non-goals

- Docker, a VM, NFS, a shared mount, or a new third-party dependency.
- A production daemon, deployment topology, storage-root activation redesign,
  schema migration, or protocol change.
- Cross-node media transfer. This epic is explicitly no-transfer.
- Exercising a real remote host in CI.
- Retaining generated fixtures or mutating an operator's source library.
- Testing issue #528's separate operator inspection and terminal-promotion
  surfaces.

## Approaches considered

### Chosen: Linux user and mount namespaces plus production processes

A checked-in shell entrypoint launches one ignored acceptance test inside a new
user and mount namespace. The test creates two backing trees, starts each
production node agent in its own child mount namespace with a different backing
tree bound at the same absolute provider paths, then covers the backing paths in
the parent/control-plane namespace before byte work starts. The control-plane
process can neither traverse the agent mount nor recover the covered backing
path by spelling its host-side name.

This is the smallest approach that provides a real OS/process boundary without
privileged host mutation or a new runtime dependency. Linux `unshare`, `mount`,
and `nsenter` are ordinary util-linux deployment tools and are preflighted by the
entrypoint. User namespaces make the mounts hermetic and disposable for an
unprivileged runner.

### Rejected: temp directories and permission bits only

The test runner and control plane share one uid. Mode bits would either remain
readable to that uid or require sudo-created host identities. A claim that a
mock identity could not read a path would not satisfy ADR 0050's OS-denial
criterion.

### Rejected: Docker, VM, or a real SSH host in CI

They add provisioning, image, networking, and cleanup state while proving no
stronger contract than private mount namespaces. A real host would make CI
non-hermetic and unavailable to contributors.

### Rejected: unit mocks or the existing owner-node emulator

The emulator is useful for durable workflow shape, but it executes in the
control-plane filesystem namespace and synthesizes worker results. It cannot
prove byte blindness, real child execution, response-loss recovery, or channel
byte accounting.

## Components

### 1. `scripts/accept-byte-blind-control-plane.sh`

The entrypoint:

- fails clearly on non-Linux hosts or missing `unshare`, `mount`, or `nsenter`;
- proves that an unprivileged user+mount namespace can be created before doing
  any build or fixture work;
- prebuilds the acceptance test and all production binaries it executes so no
  binary is relinked while an agent may exec it;
- enters one private user/mount/PID namespace and invokes only the ignored
  acceptance test;
- leaves no host mount, user, network, database, or fixture state behind.

The script is the actual smoke command and receives a `just` recipe. The test is
ignored in the normal cross-platform workspace suite because it intentionally
requires Linux namespaces and installed media tools. A dedicated Linux CI job
runs the recipe; macOS remains a normal `just ci` target.

### 2. `voom-conformance` two-host harness

A Linux-only ignored integration test owns the acceptance lifecycle. It uses
`voom-conformance` because that crate already owns black-box worker protocol
proofs. Production crates are development-only dependencies; the crate's normal
architecture edges remain unchanged.

The harness has four focused helpers:

- `NamespaceFixture` creates generated media and the two private backing trees,
  starts agent processes with private mount views, covers every backing/provider
  path in the control-plane namespace, proves denial, and restores the mounts in
  `Drop`/explicit shutdown.
- `ProcessGuard` bounds startup and shutdown, captures bounded diagnostics, and
  kills/reaps every child on failure.
- `ControlChannelAccounting` is axum middleware on the production API router. It
  counts actual request and delivered response body bytes by route and direction,
  validates each non-empty body as JSON, looks for a runtime media canary in raw,
  hexadecimal, and base64 form, and can discard exactly one committed response.
  It stores counts and booleans, never bearer-token bodies.
- `Cli` executes the installed `voom` binary against the real SQLite database,
  requires exactly one JSON envelope, and returns the parsed envelope plus exit
  status. It does not reproduce orchestration logic.

Every asynchronous wait has a named deadline. The test runs serially inside its
private namespace and does not use global host ports, fixed temp paths, ambient
configuration, or paused Tokio time with SQLite.

### 3. Generated media and reference policy

The harness generates two small Matroska libraries with FFmpeg and MKVToolNix.
Both contain `feature.mkv` at the same provider-relative locator, but their
content differs. Agent A's feature contains video, English 5.1 audio, commentary
or alternate audio, and a forced English subtitle. It also carries a generated
high-entropy non-font attachment whose prefix is the runtime network canary.
The attachment is large enough that a media transfer cannot hide inside the
measured control-body total. Agent A initially also contains `retire-me.mkv` for
scan reconciliation.

The checked-in acceptance policy combines production reference-policy portions:

- audio transcode from `audio-transcode-eac3.voom`;
- audio downmix synthesis from `audio-synthesize-downmix.voom`;
- English/non-commentary track ordering and defaults from
  `filter-addressed-tracks.voom`;
- container/remux and non-font attachment selection from
  `production-normalize-reduced.voom`; and
- final verification from `verify-artifact.voom`.

The policy uses only published grammar. The test creates it through `voom policy
create`, creates a root-scoped input set through
`voom policy input create-from-scan`, and executes it through
`voom compliance execute`. Scan requests likewise use `voom scan`. Setup-only
root activation calls the existing production `ControlPlane::activate_library_root`
case because root validation has no installed operator CLI command; this is a
test seam around an existing ADR 0055 transition, not alternate scan or workflow
orchestration.

### 4. Fault controller

The harness performs faults at real boundaries:

- **Stale heartbeat:** kill agent B without deactivation, wait past its real
  heartbeat TTL, and run production `ControlPlane::remote_recover` at the real
  clock. The owner and root become unavailable. A replacement incarnation then
  reactivates execution.
- **Incomplete scan:** after an initial successful scan, rename
  `retire-me.mkv` to a non-scanned suffix inside agent A's mount namespace. Arm
  middleware to hold the first accepted scan-batch response, kill agent A while
  that response is held, and start a replacement incarnation. Incarnation
  fencing makes the partial session stale. The old location remains live. A
  successful replacement scan retires it, after which cleanup restores the file.
- **Slow post-dispatch work:** configure agent A's FFmpeg dependency through a
  generated executable wrapper that sleeps only for transform invocations and
  then execs the real FFmpeg. The delay exceeds the configured initial five-second
  lease TTL. Lease heartbeat traffic must be observed while the audio workflow
  completes and synthesis reaches `committed` with no live expired claim.
- **Response loss and restart:** arm middleware immediately before workflow
  execution. After the API has durably applied the first lease-complete mutation,
  discard that response body and notify the harness. Kill agent A and immediately
  start a replacement incarnation. The workflow must converge with one physical
  output per committed target and one durable commit/intention record per target.

No fault helper writes database state directly. SQL is used only after production
paths settle, to observe durable invariants and counts.

## Acceptance observations

### C1 — OS-denied complete workflow

After private agent mounts exist, the parent bind-mounts an empty denial tree over
both backing roots and every configured provider path. Opening the known feature
path from the control-plane namespace must return `NotFound` or `PermissionDenied`.
Only then may scan or compliance work begin. A successful CLI workflow, output
probe from inside the current owner namespace, and completed durable phase chain
prove the workflow.

### C2 — equal paths remain distinct

Both library roots persist the identical provider locator and the identical
`feature.mkv` relative locator under different owner/root IDs. Real scans publish
different content hashes. Assertions require two live location rows, distinct
root IDs, distinct file versions/hashes, and no cross-owner alias.

### C3 — fail before dispatch

Natural polling by agent B must never lease agent A's scan/media tickets; durable
lease/access-plan owner evidence names agent A only. The harness then temporarily
points A's output default at B's root. `voom compliance execute` must fail before
any new lease or output appears. Restoring the valid A-owned defaults is setup for
the success workflow, not a retry shim.

### C4 — incomplete scan is non-destructive

The stale partial session has accepted observations but no successful completion
watermark. `retire-me.mkv` stays live after fencing. The next complete scan sees it
absent, reports one retirement, and records the retirement against that successful
session.

### C5 — post-dispatch delay preserves claims

The configured transform delay is greater than the initial lease TTL. The
accounting middleware must observe lease-heartbeat requests during that interval.
The terminal workflow is successful, synthesis state is `committed`, and no
terminal workflow row reports claim expiry.

### C6 — response loss and restart are idempotent

Exactly one response is discarded after its mutation is applied. The replaced
incarnation is durable and prior incarnation fenced. For every target locator,
there is exactly one completed commit intent/record and one output file; no target
has duplicate live location or artifact rows.

### C7 — only control traffic crosses the channel

Every API request and delivered response body is counted around the production
router. Every non-empty body parses as JSON, no body contains the runtime canary
raw or encoded, the largest body remains below the protocol's existing response
bound, and total delivered body bytes remain below the generated source-media
bytes. The final test diagnostic reports request, response, discarded-response,
message, and maximum-body counts. This attributes bytes to protocol metadata and
makes a binary/base64 media transfer fail without inspecting source code.

### C8 — cleanup preserves source

The harness hashes a relative-path manifest of both generated source libraries
before denial. After all processes stop, it restores the temporary renamed file,
unmounts the denial covers, and requires the manifest to match byte-for-byte.
Temporary directories then remove the entire generated environment.

The operator runbook follows the same invariant for a real host: snapshot the
source manifest, generate only beneath a uniquely named test directory, direct
all staging/output paths to separately created run directories, restore any
fault-renamed file, rescan, compare the manifest, and remove only the run ID's
owned paths. Cleanup is idempotent and refuses an empty or unexpected run root.

## Error handling and diagnostics

- Missing tools or namespace support fail before fixture creation and name the
  missing prerequisite.
- Every command error includes the operation, bounded stderr, exit status, and
  parsed CLI error envelope when present; tokens and request bodies are never
  printed.
- Startup waits identify the process and expected readiness fact. Runtime waits
  identify the criterion being proven.
- Cleanup attempts all child termination, mount restoration, and file restoration
  steps even after a prior failure. A cleanup error fails the test rather than
  claiming the source was preserved.
- The script uses `set -euo pipefail`; it never uses `--no-verify`, ambient SSH,
  host users, sudo, or destructive wildcard cleanup.

## Threat model

### Boundaries and actors

1. **Generated fixture → media tools.** The harness is the trusted producer;
   filenames and arguments are fixed, and dynamic paths are passed as argv, not
   shell-interpolated commands.
2. **Control plane/API → authenticated node agent.** The production bearer,
   incarnation, idempotency, strict JSON, capability/grant, owner, and heartbeat
   checks remain authoritative. The test middleware may observe byte counts but
   never logs bodies or tokens.
3. **Node agent → child workers/filesystem.** Production closed-environment,
   exact-version handshake, root binding, locator containment, expected-fact,
   and no-overwrite controls remain authoritative.
4. **Harness → mount namespace.** The local test process is trusted; the
   acceptance claim is about accidental or compromised control-plane byte access,
   not defense from a namespace root that intentionally unmounts its guard.
5. **Operator runbook → remote host.** The operator is trusted, but existing
   library data is not expendable. A unique run ID, path-prefix checks, quoted
   argv, manifest comparison, and explicit owned-path list bound cleanup.

### Added or widened boundaries

No production entry point is added or widened. The Linux-only test script and
ignored test are developer/CI entry points. They accept no untrusted network
input and generate their own paths and media. The dedicated CI job receives no
secret and has read-only repository contents plus its ordinary job token.

### Controls

- Namespace commands use argument arrays or fixed shell variables with strict
  quoting; no evaluated payload enters a command.
- The test records only aggregate channel facts; bearer values and JSON bodies
  are not retained in failure output.
- Body accounting is bounded by the production API/client limits before any
  marker scan.
- Faults are single-use state transitions and named waits, so retries cannot
  silently create an unbounded loop.
- All network listeners bind ephemeral loopback addresses.
- Cleanup verifies the generated root prefix and exact source manifest before
  deleting only temporary state.

### Explicitly out of scope

- A malicious root-equivalent CI job can unmount namespace guards; the harness
  proves production code has no required filesystem dependency, not sandbox
  resistance to its own test orchestrator.
- Confidentiality against authenticated node agents is not claimed; owner agents
  are the ADR 0050 host trust boundary.
- TLS is covered by existing API/agent suites. This hermetic test uses allowed
  loopback cleartext so middleware can attribute JSON payload bytes.
- The real-host runbook does not automate SSH credentials or host provisioning.

## Verification and CI

Focused TDD proceeds in four slices:

1. namespace/process denial and equal-path real scans;
2. stale heartbeat and incomplete-scan reconciliation;
3. owner-local/mixed-owner pre-dispatch gates plus slow workflow;
4. response loss/restart, network accounting, durable/output uniqueness, and
   cleanup/runbook assertions.

The proof commands are:

- `just accept-byte-blind` — actual namespace/process smoke harness;
- `cargo test -p voom-conformance --test byte_blind_two_host -- --ignored`
  only inside the namespace entrypoint;
- targeted unit/self-tests for shell preflight or pure accounting helpers if
  introduced;
- `just ci` — unchanged cross-platform full guardrail suite.

A dedicated Linux CI job runs `just accept-byte-blind`. Normal `just ci` remains
cross-platform and full-suite-safe. The branch is complete only when the actual
harness has run successfully, focused checks pass, and `just ci` is green.

## Durable workflow facts

- `BASE_BRANCH`: `main`
- branch: `feat/byte-blind-acceptance-425`
- host architecture: `x86_64`; targets: none declared; relationship:
  `no-target-declared`
- required full guardrail: `just ci`
- actual acceptance smoke: `just accept-byte-blind`
- ADR index coupling: coupled within `just ci`; no ADR is created by this change
