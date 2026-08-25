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

### Chosen: a namespace supervisor with capability-isolated production roles

A checked-in shell entrypoint launches one ignored acceptance supervisor inside
a disposable outer user/mount/PID namespace. The supervisor creates the generated
fixtures, two owner mount namespaces, and a separate control-plane child
user+mount+PID namespace with a private procfs. Agent mounts are owned by the
supervisor's user namespace; the control-plane child uses a newly mapped nested
user namespace and therefore has no capability in that ancestor, so it cannot
join an owner mount namespace, unmount the denial covers, or recover a backing
alias. Its PID namespace sees only the control-plane namespace init and
descendants, so owner anchor and agent PIDs from the outer namespace have no proc
entries or proc-root aliases. The two agents still see different backing trees
at the same provider paths.

The supervisor performs only fixture, namespace, signal, and cleanup mechanics.
It bind-mounts each live owner's mount-namespace handle onto one allowlisted
mechanics-only file before nesting the control-plane role. The isolated role runs
the real SQLite/control-plane/API/CLI paths and exchanges bounded typed commands
with the supervisor over a pre-opened local socket for fault timing. Before byte
work, that role proves ordinary path denial, proves that `nsenter
--mount=<allowlisted-handle>` fails with `EPERM`, closes the handles, and proves
that each supplied outer owner PID is absent from its private procfs and that
opening `/proc/<owner-pid>/root` fails with `ENOENT`. The reachable handles make
the capability assertion independent of the deliberately hidden outer proc
entries without exposing an owner root or byte-bearing file descriptor.

This is the smallest approach that provides a real OS/process boundary without
privileged host mutation or a new runtime dependency. Linux `unshare`, `mount`,
and `nsenter` are ordinary util-linux deployment tools and are preflighted by the
entrypoint. User namespaces make the hierarchy hermetic and disposable for an
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
- proves that both the outer user+mount namespace and a newly mapped nested user
  namespace with a private PID/proc view can be created before doing any build or
  fixture work;
- prebuilds the acceptance test and all production binaries it executes so no
  binary is relinked while an agent may exec it;
- starts outer `unshare` as a tracked child with `--fork`, a PID namespace, and
  `--kill-child=SIGKILL`; normal completion waits for it, while `SIGINT`/`SIGTERM`
  traps send `SIGKILL` to the monitor and wait for that direct child so its
  kill-child contract synchronously destroys the PID namespace and every
  descendant instead of forwarding a signal the monitor ignores;
- enters one private user/mount/PID namespace and invokes only the ignored
  acceptance test;
- requires the supervisor to track the nested control-plane `unshare` monitor,
  whose `--map-root-user --mount --pid --fork --mount-proc
  --kill-child=SIGKILL` lifecycle creates the required capability hierarchy;
  normal completion waits for the role, while cancellation kills and waits for
  the monitor before owner anchors, with every wait bounded by `ProcessGuard`;
- leaves no host mount, user, network, database, fixture, or live process state.

The script is the actual smoke command and receives a `just` recipe. The test is
ignored in the normal cross-platform workspace suite because it intentionally
requires Linux namespaces and installed media tools. A dedicated Linux CI job
runs the recipe; macOS remains a normal `just ci` target.

### 2. `voom-conformance` two-host harness

A Linux-only ignored integration test owns the acceptance lifecycle. It uses
`voom-conformance` because that crate already owns black-box worker protocol
proofs. Production crates are development-only dependencies; the crate's normal
architecture edges remain unchanged.

The harness has five focused helpers:

- `NamespaceSupervisor` creates generated media and two private backing trees,
  starts one long-lived owner-mount anchor per agent, covers every backing/provider
  alias outside those owner views, and starts/restarts agents. It bind-mounts one
  allowlisted persistent handle for each owner mount namespace, launches and
  tracks the nested control-plane namespace monitor, and never runs a
  control-plane case or workflow.
- `ControlPlaneRole` re-execs the exact integration-test binary inside a newly
  mapped nested user+mount+PID namespace with a private procfs. The nested
  namespace is launched through a tracked `unshare --map-root-user --mount --pid
  --fork --mount-proc --kill-child=SIGKILL` monitor. On cancellation the
  supervisor kills and boundedly reaps that monitor, which fires its kill-child
  contract, instead of relying on the monitor's ignored `SIGINT`/`SIGTERM` or PID
  1's default signal semantics. The role runs real SQLite, the production API
  router, and installed CLI processes. A bounded framed local socket requests
  only named supervisor mechanics (pause/resume/restart, owner-view
  rename/restore/probe). Before dispatch, the role must use each allowlisted
  persistent namespace handle to demonstrate `EPERM`, close the handles,
  demonstrate that both outer owner PIDs are absent from its procfs, and
  demonstrate `ENOENT` when opening either `/proc/<owner-pid>/root`.
- `ControlChannelAccounting` wraps the production API listener's accepted TCP
  streams to count exact HTTP wire bytes, and layers typed axum middleware on the
  production router to attribute methods, targets, headers, statuses, and bodies.
  The middleware validates each route/direction payload, looks for every generated
  byte object's marker in raw and encoded forms, and implements the two single-use
  response/hold faults. It stores aggregate categories and booleans, never
  bearer-token or fence bodies.
- `Cli` executes the installed `voom` binary against the real SQLite database,
  requires exactly one JSON envelope, and returns the parsed envelope plus exit
  status. It does not reproduce orchestration logic.

Every asynchronous wait has a named deadline. The supervisor and isolated
control-plane role run one serial scenario and do not use global host ports,
fixed temp paths, ambient configuration, or paused Tokio time with SQLite.

### 3. Generated media and reference policy

The harness generates two small Matroska libraries with FFmpeg and MKVToolNix.
Both contain `feature.mkv` at the same provider-relative locator, but their
content differs. Agent A's feature contains video, English 5.1 audio, commentary
or alternate audio, and a forced English subtitle. Every generated media object
carries its own high-entropy font attachment: a runtime marker is repeated at
short fixed intervals through a large attachment, so the attachment is preserved
by the reference selection policy and any meaningful raw or encoded media chunk
is visible to channel accounting. The smallest complete media object is larger
than the whole measured control-body total. Agent A initially also contains
`retire-me.mkv` for scan reconciliation.

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
- **Slow post-dispatch work:** after the transform child, agent-side fact checks,
  and staged-output probe have returned, middleware holds the original
  lease-complete request before the API applies it. The hold exceeds the initial
  five-second TTL while the agent's independent heartbeat task must keep that
  exact lease alive. After release, the API applies the original completion and
  durable evidence must retain the same lease, attempt, synthesis claim token,
  and claim generation through verification and commit, with successful
  heartbeat responses and no expiry or requeue fact.
- **Response loss and restart:** arm middleware immediately before workflow
  execution. After the owner has performed the add-only filesystem mutation,
  durably recorded its `applied` receipt, and the API has applied the matching
  `/v1/artifact/commit/{intent_id}/outcome` mutation, discard that response body
  and notify the harness. Kill agent A and immediately start a replacement inside
  the pinned owner namespace. Recovery must retain the same intent/generation and
  converge with one physical output, commit intent, commit record, applied receipt,
  and live target location per target.

No fault helper writes database state directly. SQL is used only after production
paths settle, to observe durable invariants and counts.

## Acceptance observations

### C1 — OS-denied complete workflow

After private agent mounts exist, the parent bind-mounts an empty denial tree over
both owner backing roots and every configured media, staging, output, backup, and
recovery provider path. Before dispatch, the harness places a distinct sentinel
beneath every provider root and every host-side backing alias. It also exposes
only two mechanics-only persistent owner mount-namespace handles, never an owner
root descriptor. The isolated role then parses its own mount table and requires
each configured path to resolve to the denial-cover mount identity, requires
every backing sentinel/alias open to fail, and requires entry through each
allowlisted namespace handle to fail with `EPERM` before closing both handles.
Its private procfs must have a distinct mount identity, must omit the supplied
outer owner-anchor and agent PIDs, and must return `ENOENT` for direct
`/proc/<owner-pid>/root` sentinel access. These are positive assertions over the
live namespace, not assumptions about the host's ptrace policy.
The cover itself may remain traversable; its emptiness, mount identity, the
nested PID/proc view, and the control-plane role's lack of capability in the
owner namespaces prove the backing bytes are OS-inaccessible.
Only then may scan or compliance work begin. A
successful CLI workflow, supervisor-requested output probe inside the current
owner namespace, and completed durable phase chain prove the workflow.

### C2 — equal paths remain distinct

Both library roots persist the identical provider locator and the identical
`feature.mkv` relative locator under different owner/root IDs. Real scans publish
different content hashes. Assertions require two live location rows, distinct
root IDs, distinct file versions/hashes, and no cross-owner alias.

### C3 — fail before dispatch

The harness pauses agent A before creating ready A-owned work, waits for one
completed acquire poll from agent B, and requires a no-candidate response with
no B-owned lease or child dispatch before resuming A. Durable lease/access-plan
owner evidence must then name A only. The harness also temporarily points A's
output default at B's root. `voom compliance execute` must fail before any new
lease or output appears. Restoring the valid A-owned defaults is setup for the
success workflow, not a retry shim.

### C4 — incomplete scan is non-destructive

The stale partial session has accepted observations but no successful completion
watermark. `retire-me.mkv` stays live after fencing. The next complete scan sees it
absent, reports one retirement, and records the retirement against that successful
session.

### C5 — post-dispatch delay preserves claims

The middleware holds the post-dispatch lease-complete request for longer than the
initial TTL. It must observe successful lease-heartbeat responses for that same
delayed lease while the hold is active. Durable assertions require one lease and
dispatch attempt, one unchanged synthesis claim token/generation through commit,
zero lease-expiry or ticket-requeue events for that workflow, no replacement
lease, successful terminal workflow state, and synthesis state `committed`.

### C6 — response loss and restart are idempotent

Exactly one post-filesystem-mutation commit-outcome response is discarded after
its durable mutation is applied. The replaced incarnation is durable and the
prior incarnation fenced. Recovery retains the same intent ID and generation.
For every target locator there is exactly one completed intent, commit record,
applied receipt, live location, and output file; no successor intent or second
provider mutation is recorded.

### C7 — only control traffic crosses the channel

Every API exchange is counted around the production router. Accounting includes
the request method/target, allowlisted route template and typed path IDs,
authorization/idempotency/content headers, response status/content headers, and
both bodies; bearer/fence values are categorized and sized without being
retained. Each observed route/direction pair must decode into its exact
production request or response type. A route-specific semantic classifier then
attributes every variable-length leaf to a bounded control category:
bearer/idempotency identity, durable IDs/epochs, root-relative locators,
operation/taxonomy tokens, expected/observed facts and hashes, bounded probe
snapshots, diagnostics, or strict operation results. It rejects an unknown
method, route, path component, status, header, field, leaf kind, or unbounded
opaque value; in particular, lease-complete `result` values are decoded by the
durable ticket's operation into the matching strict worker or scan result before
their leaves are classified. Per-object raw/hex/alignment-safe base64 markers
and whole-channel totals remain independent backstops, not the attribution
proof. The final diagnostic reports request, response, discarded-response,
message, route/category, maximum-leaf, header, body, exact stream-byte, and
HTTP-framing-overhead counts. Every application-controlled value is attributed
as protocol metadata without a source-text assertion.

### C8 — cleanup preserves source

The harness hashes a relative-path manifest of both generated source libraries
before denial. Namespace anchors remain alive while cleanup restores the
fault-renamed file and compares the source manifest from the owner view. It then
stops agents, removes the denial covers and persistent namespace-handle mounts,
compares the manifest again from the parent view, stops the anchors, and removes
the generated temporary environment.

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
- Cleanup attempts all child termination, namespace-handle unmount, denial-cover
  unmount, and file restoration steps even after a prior failure. A cleanup
  error fails the test rather than claiming the source was preserved.
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
4. **Supervisor → isolated roles.** The supervisor is the trusted test
   orchestrator and retains namespace authority; it runs no control-plane logic.
   The actual control-plane/API/CLI role is capability-isolated from owner mounts
   and PID/proc views and can request only the bounded mechanical commands on the
   local control socket. Its two mechanics-only namespace handles expose no root
   or byte-bearing descriptor and are closed immediately after the `EPERM` proof.
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

1. nested namespace/process/proc-root denial and equal-path real scans;
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
