# Byte-blind two-host acceptance — implementation plan

Issue #425 · spec:
`docs/workflow/specs/2026-08-24-byte-blind-two-host-acceptance-design.md` ·
accepted ADRs 0050, 0055, 0075, 0076, and 0077

## Goal and architecture

Prove the eight issue #425 criteria by driving generated Matroska media through the
production CLI, control-plane cases, API router, node agents, and real media workers while
the control-plane role has no OS path, owner PID, proc-root, or mount-namespace access to
owner bytes. A synchronous namespace supervisor owns only fixtures, process mechanics,
and cleanup; the exact integration-test binary re-execs as owner-anchor and nested
control-plane roles. The control plane serves the production API router on loopback with a
counting transport and typed fault middleware; owner agents resolve the same absolute
provider path to distinct private backing trees.

Tech stack: Rust 2024 on the workspace Rust 1.95 floor; tokio, axum, hyper, sqlx, serde,
and tempfile already pinned in the workspace; Bash with util-linux `unshare`, `mount`, and
`nsenter`; FFmpeg, ffprobe, and MKVToolNix already provisioned by CI.

## Global constraints

- Preserve the spec's no-transfer contract: no media body, path alias, `/proc` root, or
  owner namespace becomes readable to the control-plane role. Network traffic is loopback
  control JSON only.
- Add no third-party dependency, container, VM, host user, sudo requirement, fixed port,
  fixed temporary path, ambient SSH call, or real-host CI dependency.
- The real acceptance test is Linux-only and ignored by ordinary `cargo test`; `just ci`
  remains cross-platform and full-suite-safe. `just accept-byte-blind` is the actual smoke
  and a dedicated Ubuntu CI job runs it.
- Every wait has a named deadline. Every child is tracked, diagnostics are bounded, normal
  shutdown is graceful where observable state matters, and cancellation kills the relevant
  `unshare` monitor before a bounded reap so `--kill-child=SIGKILL` destroys its PID
  namespace.
- The control-plane namespace launch is exactly a newly mapped nested user namespace plus
  private mount/PID/proc views: `unshare --map-root-user --mount --pid --fork
  --mount-proc --kill-child=SIGKILL`.
- Before the first scan, the control-plane role positively asserts denial-cover mount
  identity, backing sentinel failure, `EPERM` through two allowlisted persistent owner
  mount-namespace handles, owner-anchor/agent PID absence, and `ENOENT` through each
  `/proc/<outer-pid>/root` path. The mechanics-only handles are then closed.
- Use generated media only. Source invariance is a sorted relative-path, size, and BLAKE3
  manifest of the original library files. Cleanup restores the fault-renamed file and
  compares the manifest both through the owner view and after denial covers are removed.
- Production setup goes through installed `voom` commands except the existing
  `ControlPlane::activate_library_root` seam, which has no installed CLI equivalent.
  SQL after production paths settle is observation only; fault helpers never mutate DB
  state.
- Match repository conventions: functions at most 100 lines, sibling unit tests for `src/`
  modules, no paused Tokio clock with SQLite, deny warnings, no unsafe code, exact JSON
  envelopes from every CLI invocation, and explicit-path commits.
- Guardrails: focused commands below, actual smoke `just accept-byte-blind`, then full
  `just ci`. ADR index coupling is already satisfied because this change adds no ADR.

## File map

- `crates/voom-conformance/Cargo.toml`: add `blake3.workspace = true` for the
  shared manifest helper; add only existing internal/workspace dev dependencies
  needed by the integration target (`voom-api`, `voom-control-plane`,
  `voom-store`, `voom-node-agent`, `voom-test-support`, `http-body-util`, hyper,
  hyper-util, sqlx, and tempfile); and pin the already-used direct test
  dependencies exactly as their owning crate does: `axum = "0.8.9"` and
  `tower = { version = "0.5.3", features = ["util"] }`.
- `crates/voom-conformance/src/source_manifest.rs` and
  `source_manifest_test.rs`: complete bounded source-tree enumeration and
  sorted relative-path/size/BLAKE3 manifests shared by the harness and runbook.
- `crates/voom-conformance/src/bin/byte_blind_manifest.rs`: checked-in
  one-root operator helper that emits the strict manifest as JSON without a new
  package; `lib.rs` exports the manifest module.
- `crates/voom-conformance/tests/byte_blind_two_host.rs`: ignored test entrypoint, role
  selection, common error/result plumbing, and end-to-end criterion ordering.
- `crates/voom-conformance/tests/byte_blind_two_host/protocol.rs`: bounded framed local
  supervisor protocol and criterion observations; no orchestration logic.
- `crates/voom-conformance/tests/byte_blind_two_host/process.rs`: `ProcessGuard`, generated
  fixtures, outer supervisor, owner mount anchors, nested control-plane launch, denial
  covers, persistent namespace handles, agent process mechanics, and idempotent teardown.
- `crates/voom-conformance/tests/byte_blind_two_host/accounting.rs`: counted TCP transport,
  production-router middleware, route/payload attribution, marker checks, and the two
  single-use faults.
- `crates/voom-conformance/tests/byte_blind_two_host/scenario.rs`: real SQLite/CLI/API/node
  setup, scans, policy workflow, real-clock waits, and read-only durable assertions.
- `scripts/accept-byte-blind-control-plane.sh`: prerequisite/user-namespace preflight,
  complete binary prebuild, outer namespace launch, and signal-safe wait.
- `justfile`: `accept-byte-blind` recipe only.
- `.github/workflows/ci.yml`: dedicated Ubuntu acceptance job using the existing pinned
  actions and existing FFmpeg/MKVToolNix provisioning.
- `docs/runbooks/byte-blind-two-host-acceptance.md`: optional `ssh homer` procedure rooted
  at `/mnt/pool0/test-video`, generated-only run paths, evidence capture, and idempotent
  cleanup.

No production crate, schema, migration, API contract, policy grammar, worker protocol, or
source fixture is planned to change. A failed acceptance slice that demonstrates a real
production defect returns to the spec/scope gate before widening this file map.

## T1 — Namespace/process proof and equal-path real scans

### Interfaces

`protocol.rs` defines and later tasks consume:

```rust
const MAX_FRAME_BYTES: usize = 1_048_576;

enum OwnerId { A, B }
enum AgentSignal { Stop, Continue, Kill }
enum SupervisorCommand {
    StartAgent { owner: OwnerId, config_path: PathBuf },
    SignalAgent { owner: OwnerId, signal: AgentSignal },
    RestartAgent { owner: OwnerId, config_path: PathBuf },
    RenameInOwner { owner: OwnerId, from: PathBuf, to: PathBuf },
    Manifest { owner: OwnerId },
    ProbeOutput { owner: OwnerId, relative_locator: PathBuf },
    CloseNamespaceHandles,
    RemoveDenialCovers,
    Finish,
}
enum SupervisorReply {
    AgentStarted { outer_pid: u32 },
    AgentSignalled,
    Renamed,
    Manifest(SourceManifest),
    OutputProbe(OutputProbe),
    NamespaceHandlesClosed,
    CoversRemoved,
    Finished,
    Failed { operation: String, message: String },
}
fn write_frame<T: Serialize>(stream: &mut UnixStream, value: &T) -> TestResult<()>;
fn read_frame<T: DeserializeOwned>(stream: &mut UnixStream) -> TestResult<T>;
```

`process.rs` provides:

```rust
struct ProcessGuard { /* child, bounded stderr, name, deadline */ }
impl ProcessGuard {
    fn spawn(name: &'static str, command: &mut Command) -> TestResult<Self>;
    fn outer_pid(&self) -> u32;
    fn signal(&mut self, signal: AgentSignal) -> TestResult<()>;
    fn wait(&mut self, deadline: Duration) -> TestResult<ExitStatus>;
    fn kill_and_wait(&mut self, deadline: Duration) -> TestResult<()>;
}
struct NamespaceSupervisor { /* temp root, anchors, handles, covers, control role */ }
impl NamespaceSupervisor { fn run() -> TestResult<AcceptanceEvidence>; }
fn run_owner_anchor() -> TestResult<()>;
fn run_control_plane_role() -> TestResult<AcceptanceEvidence>;
```

`AcceptanceEvidence` is a typed aggregate with one field per C1–C8 observation. It is
constructed only after every criterion passes, printed once without tokens/bodies, and is
not a substitute for assertions at the real boundary.

### TDD steps

1. Add the integration target and protocol tests that reject a frame larger than
   `MAX_FRAME_BYTES`, an unknown command/field, a truncated length prefix, and a manifest
   path containing `..` or an absolute component. Run
   `cargo test -p voom-conformance --test byte_blind_two_host protocol -- --ignored`; expect
   failures because the framed protocol and role dispatcher do not exist.
2. Implement the strict protocol with `#[serde(deny_unknown_fields)]` request/response
   content structs and exact-length reads. Re-run the command; expect the protocol tests to
   pass.
3. Add the ignored C1/C2 scenario first: two generated backing trees each contain
   `feature.mkv` at the same relative locator but with distinct runtime marker attachments;
   A also contains `retire-me.mkv`. The test must fail until owner namespaces, covers, and
   the nested control-plane role are wired.
4. Implement fixture generation with argv-only `Command` calls: FFmpeg creates bounded
   video, English 5.1/alternate audio, and forced English subtitle streams; MKVToolNix adds
   a large repeated high-entropy font attachment. Record marker raw/hex/base64 forms and
   require the smallest complete media object to exceed the later control-body total.
5. Implement complete manifest enumeration in `source_manifest.rs` first. Its
   tests cover sorted output, changed/missing/added files, symlink rejection,
   path containment, and byte/entry bounds. Implement owner anchors next. Each
   starts under a private mount namespace, makes mount propagation private,
   bind-mounts its backing tree at the identical absolute provider path,
   connects to its unique local supervisor socket, and owns agent
   start/signal/restart, owner-view rename, full source-root manifest, and
   output-probe commands. It never opens SQLite.
6. Implement the outer supervisor. Start both anchors before covering the backing aliases;
   bind each `/proc/<anchor-pid>/ns/mnt` to one mechanics-only persistent handle; bind an
   empty denial tree over provider and backing aliases in the supervisor view; launch the
   nested mapped user/mount/PID/proc control-plane role; serve only typed commands; always
   unmount covers/handles and reap children in reverse order.
7. Implement C1 live assertions in the nested role before scan: parse
   `/proc/self/mountinfo` and compare mount IDs; open every sentinel/alias and
   require `ENOENT` or `EACCES`; run `nsenter --mount=<handle> -- /bin/true` and
   require `EPERM`; send `CloseNamespaceHandles` and require the supervisor's
   acknowledged unmount before any scan; require both anchor and returned agent
   outer PIDs absent from `/proc`; and require
   `/proc/<pid>/root/<sentinel>` to fail with `ENOENT`.
8. Initialize SQLite with `voom init`, create two nodes and one durable library
   containing two differently owned roots with the identical provider locator,
   activate roots through `ControlPlane::activate_library_root`, write strict
   agent configs in the shared mechanics directory, and start production
   node-agent processes. Worker programs are the prebuilt
   scan/hash/ffprobe/ffmpeg/mkvtoolnix/verify binaries. A generated wrapper
   supplies the absolute `VOOM_MKVMERGE_BIN` to the unchanged production
   MKVToolNix worker; all other tool paths use existing agent dependency fields.
9. Start the production router on an ephemeral loopback listener and run real `voom scan`
   for both roots. Assert live location rows have equal provider/relative locator strings,
   distinct owner/root IDs, and distinct hashes/file versions.
10. Add `scripts/accept-byte-blind-control-plane.sh`: preflight Linux and every executable;
    run an outer+nested user namespace probe before building; prebuild all target binaries;
    trap INT/TERM by killing and waiting for the outer unshare monitor; run only the ignored
    test in the outer private namespace. Add `just accept-byte-blind`.
11. Run `just accept-byte-blind`; expect C1 and C2 to pass with a diagnostic naming two
    distinct hashes and zero reachable owner proc roots. Commit the green slice as
    `test: prove namespace-denied equal-path scans`.

### Acceptance

C1 denial is observed in the same nested role that owns SQLite/API/CLI, not in a helper
namespace. C2 uses real scan/hash/probe workers on real generated media. Cancelling the
script or a failing assertion leaves no process or mount visible outside the outer user
namespace.

## T2 — Stale heartbeat and incomplete-scan reconciliation

### Interfaces

`accounting.rs` begins here with the production API listener shared by all later tasks:

```rust
struct ChannelState { /* aggregates, token digests, faults, notifications */ }
struct CountingIo { /* TcpStream + atomic read/write counters */ }
struct CountingServer { address: SocketAddr, shutdown: oneshot::Sender<()>, task: JoinHandle<_> }
impl CountingServer {
    async fn start(router: Router, state: Arc<ChannelState>) -> TestResult<Self>;
    async fn shutdown(self) -> TestResult<ChannelReport>;
}
enum FaultArm { HoldScanBatch { node_id: NodeId }, HoldLeaseComplete { operation: OperationKind }, DropCommitOutcome }
```

The accepted-loop wraps each `TcpStream` in `CountingIo` and serves the exact
`router_with_control_plane` via hyper/tower. Middleware collects bounded request/response
bodies, rebuilds them unchanged except for the named single-use fault, and never stores
bearer/fence values or complete bodies.

### TDD steps

1. Add an accounting self-test with a local production health route that proves exact stream
   reads/writes exceed attributed header+body bytes by positive HTTP framing overhead, rejects
   an unallowlisted route/header, and never stores an authorization value. Run the focused
   integration target; expect failure before the counting listener exists, then implement
   the minimum listener/middleware and make it pass.
2. After both initial scans, kill agent B without graceful deactivation, wait past its real
   heartbeat TTL, call production `ControlPlane::remote_recover(SystemClock::now())`, and
   assert B's incarnation is stale/superseded and B's root is unavailable. Restart B through
   its pinned owner anchor, wait for fresh activation, and reactivate its root.
3. Rename A's `retire-me.mkv` to `retire-me.mkv.hidden` through the owner anchor. Arm one
   post-apply hold for A's first scan-batch response and invoke `voom scan --root <A>
   --no-wait`. Wait until middleware confirms the batch mutation committed, kill A while the
   response is held, start a replacement incarnation, and release the dead response.
4. Assert the stale partial session has accepted observations but no completion watermark and
   `retire-me.mkv` remains live. Invoke a fresh blocking `voom scan --root <A>`; assert it
   reports one retirement tied to the successful replacement session. Keep the file renamed
   until final cleanup.
5. Run `just accept-byte-blind`; expect C4 and stale-heartbeat observations plus C1/C2 to pass.
   Commit as `test: prove fenced scan reconciliation`.

### Acceptance

No helper updates scan/session/location tables. The partial session is made incomplete by a
real applied-response boundary and incarnation replacement; the next production scan alone
retires the absent location.

## T3 — Pre-dispatch owner gates and slow-work claim continuity

### Interfaces

`scenario.rs` adds:

```rust
struct Cli { binary: PathBuf, database_url: String, local_node_id: NodeId }
impl Cli {
    fn run(&self, args: &[OsString], deadline: Duration) -> TestResult<CliEnvelope>;
    fn spawn(&self, args: &[OsString]) -> TestResult<ProcessGuard>;
}
async fn wait_for<F, Fut>(name: &'static str, deadline: Duration, observe: F) -> TestResult<()>
where F: FnMut() -> Fut, Fut: Future<Output = TestResult<bool>>;
```

`Cli::run` requires one JSON envelope, validates command/status against the
requested operation, captures bounded stderr, and rejects extra stdout. The
runtime policy copies only published grammar and uses a dependency-ordered,
single-mutation graph: `audio_transcode` transcodes English/undefined audio to
E-AC-3; `audio_downmix` depends on it and synthesizes the 5.1-to-stereo
companion; `normalize` depends on the downmix and performs the one MKV remux
with English/non-commentary ordering/defaults plus font selection; `verify`
depends on normalize and verifies the staged artifact. Each phase consumes its
predecessor's produced artifact, and no phase contains two audio/remux/video
mutations.

### TDD steps

1. Create the policy and a root-scoped input set through installed CLI commands. Add a failing
   C3 check: pause agent A via the owner anchor before execution creates ready A-owned work;
   require middleware to observe a completed B acquire poll after work is ready; assert the
   response is idle, no B lease/dispatch exists, and durable access-plan/lease owner evidence
   names A only. Resume A.
2. Temporarily update A's output default to B's root through the production library-root CLI.
   Run `voom compliance execute`; require an error envelope before any new lease, child
   dispatch, intent, or output. Restore A's output/staging defaults through the same CLI and
   only then start the success run.
3. Arm `HoldLeaseComplete` for the audio-synthesis operation. Spawn real `voom compliance
   execute`; middleware must hold the original completion request before router mutation,
   record that transform/fact/probe result was received, and continue serving the same
   agent's lease-heartbeat requests.
4. Hold longer than the configured five-second initial lease TTL using real time. Require at
   least two successful heartbeats for that exact lease, then release the original request.
   Wait for this first successful workflow to continue through remux,
   verification, and add-only commit.
5. Observe durable rows without mutation: one lease and dispatch attempt for
   the held synthesis work; the same synthesis operation, dispatch generation,
   claim token, and claim generation from acquisition through commit; no
   expiry/requeue/replacement lease fact; terminal workflow success; synthesis
   state `committed`; completed policy phases include real transcode,
   synthesized companion, remux/track selection, and verification results.
6. Run `just accept-byte-blind`; expect C3 and C5 plus prior criteria to pass. Commit as
   `test: prove owner gates and live slow-work claims`.

### Acceptance

The wrong-owner and mixed-root failures happen before dispatch and are proven by both channel
and durable counts. The delay starts after agent-side byte work and exceeds the original TTL;
claim continuity is exact identity continuity, not merely eventual success.

## T4 — Lost response/restart idempotency, control-only bytes, cleanup, and operator path

### Interfaces

`ChannelReport` contains only aggregate values:

```rust
struct ChannelReport {
    request_count: u64,
    response_count: u64,
    discarded_response_count: u64,
    stream_read_bytes: u64,
    stream_written_bytes: u64,
    request_header_bytes: u64,
    response_header_bytes: u64,
    request_body_bytes: u64,
    response_body_bytes: u64,
    max_leaf_bytes: usize,
    routes: BTreeMap<RouteCategory, RouteTotals>,
    marker_observed: bool,
    unknown_value_observed: bool,
}
```

The route classifier allowlists every production path/method/status/header actually observed,
decodes each request/response into its strict route shape, and recursively classifies each
variable leaf as identity/epoch, rooted locator, operation/taxonomy, expected/observed fact or
hash, bounded probe snapshot/diagnostic, or strict operation result. Unknown routes, fields,
leaf kinds, opaque values, or bounds fail immediately. Raw/hex/alignment-safe base64 marker
search and the smallest-media-size comparison remain independent backstops.

### TDD steps

1. Snapshot durable/output counts, arm `DropCommitOutcome`, and start a second
   execution of the same generated-media policy/input. After the owner has
   created the new target file and applied receipt and the production
   `/v1/artifact/commit/{intent_id}/outcome` handler has durably applied the
   mutation, middleware counts but replaces exactly that response body with an
   empty body and notifies the role.
2. Kill agent A immediately and start a replacement in the same owner mount
   namespace. Wait for replacement activation, resolve the dropped intent's
   artifact handle through read-only observation, and invoke the installed
   `voom artifact recover-commit --artifact-handle-id <id>` path. Require its
   success envelope before waiting for convergence. Assert the old incarnation
   is fenced, the new one is current, and the intent/generation is unchanged.
3. Relative to the pre-run snapshot, for every target locator assert exactly
   one physical output, completed intent, commit record, applied receipt, live
   target location, and provider mutation; assert no successor
   intent/generation or duplicate durable record. This closes C6.
4. Complete the route classifier for every exchange observed by the full scenario. Assert
   exact read/write totals, positive framing overhead, no unknown value, no marker in raw/hex/
   any base64 alignment, and total control bodies smaller than the smallest media object.
   Emit only aggregate route/category/header/body/stream diagnostics. This closes C7.
5. Restore `retire-me.mkv` through owner A, rescan successfully, and compare
   the complete original source manifest through each live owner anchor. Stop
   agents, remove denial covers, compare the complete enumerated path/size/hash
   sets from the supervisor view, stop anchors, and let the unique temp root
   disappear. Cleanup tolerates already-unmounted namespace handles and repeats
   safely in the test's error path. This closes C8.
6. Add `byte_blind_manifest`, whose exact interface is
   `byte-blind-manifest <source-root>` and whose only stdout is the strict JSON
   manifest produced by `SourceManifest::build`. Add the operator runbook. It
   prebuilds that existing-package helper locally, copies or invokes it on
   `homer`, creates
   `/mnt/pool0/test-video/voom-byte-blind-$RUN_ID/{source,staging,output,backup}`,
   refuses empty/non-matching run roots, records the helper's complete sorted
   BLAKE3 source manifest, uses generated media, records the same
   CLI/API/durable evidence, restores renamed input, rescans, compares the
   manifest byte-for-byte, and removes only the exact run-ID paths. Running
   cleanup twice is a no-op; the runbook never deletes an existing library
   path.
7. Add a dedicated Ubuntu `byte-blind-acceptance` CI job to `.github/workflows/ci.yml` with
   `permissions: contents: read`, existing SHA-pinned checkout/cache/just actions, existing
   FFmpeg/MKVToolNix install, and `just accept-byte-blind`. Do not path-filter the job.
8. Verify in order:
   - `cargo test -p voom-conformance --test byte_blind_two_host protocol -- --ignored` — all
     pure protocol/accounting cases pass;
   - `shellcheck scripts/accept-byte-blind-control-plane.sh` and
     `shfmt -d scripts/accept-byte-blind-control-plane.sh` — no findings;
   - `actionlint` and `zizmor .github/workflows/ci.yml` — no findings;
   - `just accept-byte-blind` — one ignored real scenario runs and reports C1–C8 true,
     nonzero control bytes, zero media markers, one dropped response, and unchanged source;
   - `just ci` — all cross-platform workspace guardrails pass.
9. Commit the runbook/CI/complete proof as `test: cover byte-blind recovery and cleanup`.

### Acceptance

The actual harness, not a source-text or mock assertion, proves every #425 criterion. The
operator procedure is optional and safe; CI remains hermetic. A failure at any point retains
bounded diagnostics, fails cleanup if invariants cannot be restored, and never claims source
preservation without both manifest comparisons.

## Review and delivery

Run the branch trial loop against `main`, explicitly checking namespace capability hierarchy,
PID/proc denial, permission assumptions, route attribution, fault placement, lease/claim
identity, process cancellation, and cleanup. Run the security pass over namespace handles,
permissions, process arguments/environment, listener exposure, token/body retention, network
measurement, and deletion-prefix checks. Apply a behavior-preserving simplification pass,
rerun `just accept-byte-blind` and `just ci`, then push and open a PR with `Closes #425`; do
not merge.

## Criterion map

- C1: T1 nested mapped user+mount+PID/proc proof plus full T3/T4 workflow.
- C2: T1 equal provider/relative locators and distinct real scan hashes.
- C3: T3 B idle acquire and mixed-root pre-dispatch failure.
- C4: T2 applied partial batch, fenced incarnation, then complete rescan retirement.
- C5: T3 held post-dispatch completion, same-lease heartbeats, exact claim continuity.
- C6: T4 post-mutation lost response, replacement agent, physical/durable uniqueness.
- C7: T2/T4 counted production listener, strict attribution, marker and size backstops.
- C8: T1 baseline plus T4 owner/supervisor manifest comparisons and runbook cleanup.
