# Sprint 17 Slice: Operator-Runnable Real-Media Execution

Date: 2026-06-05
Status: Approved (5 challenge cycles addressed; whole-scan builder scope addition confirmed)
Branch: `feat/sprint-17-operator-execution-slice`

## Context

The real-media pipeline (scan, policy compile/plan, compliance report/apply,
remux, video/audio transcode, artifact stage/verify/commit) is implemented and
integration-tested through Sprint 16. However, it is **not runnable end-to-end
from the shipped `voom` CLI**. Two pieces of connective tissue exist only in
`voom-test-support`:

1. **No operator path to launch and register real mutation workers.**
   `compliance execute` discovers workers by reading `{endpoint, secret}` from
   `worker_capabilities.extra` (`policy_runtime_registry()` /
   `runtime_metadata()` in `crates/voom-control-plane/src/cases/policy/compliance.rs`).
   The CLI `voom worker register` hardcodes `extra: json!({})`
   (`crates/voom-cli/src/commands/execution/worker.rs`), so a CLI-registered
   worker has no endpoint, is skipped by the registry, and `execute` finds zero
   runtimes. The only code that launches a worker binary and records its live
   endpoint is `TestWorkerLaunch::start` in
   `crates/voom-test-support/src/worker.rs`.

2. **No CLI path to author a policy.** `compliance execute` requires
   `document.current_accepted_version_id == policy_version_id`. The control plane
   has `create_policy_document` / `add_policy_version` (both auto-accept the new
   version via `advance_current_version`), but the CLI `PolicyCommand` exposes
   only `input create-from-scan`. Authoring is reachable only from tests.

### Roadmap placement

This work is inside the **Real Media CLI milestone** (Sprints 10-17), which must
close before any daemon sprint (`docs/specs/voom-control-plane-design.md`, lines
1696-1701). Sprint 16 closed 2026-06-05; Sprint 17 has no design yet.

- Policy authoring CLI was a **Foundation CLI milestone** deliverable
  ("create and list policy documents and versions", line 1563) and is re-listed
  in Sprint 17 (line 2096). It slipped from Foundation; Sprint 17 is its backstop.
- Sprint 16's acceptance ("a multi-phase policy ... executed and inspected
  through CLI JSON envelopes", lines 2078-2080) was demonstrated in
  `crates/voom-cli/tests/multi_phase_flow.rs`, which uses `TestWorkerLaunch` to
  launch and register the real ffmpeg worker, then shells out to the real `voom`
  binary. The CLI execute path works **given** registered workers; the
  operator-facing way to launch and register them was never shipped.
- The spec forbids the daemon from being the first interface for mutating durable
  control-plane state (lines 1550-1556) and requires every daemon-consumed
  durable state family to have an explicit CLI creation path (lines 1609-1614).
  Registered workers-with-endpoints are such a family, so this must exist before
  Sprint 18.

This slice is therefore the highest-value part of Sprint 17: it makes the whole
real-media pipeline operator-runnable. The remainder of Sprint 17 is deferred to
follow-on slices.

## Goal

Make the real-media pipeline runnable end-to-end from the `voom` CLI: author a
policy, stand up real mutation workers, and run scan -> remux-to-MKV ->
transcode-to-HEVC against a real directory, entirely through JSON envelopes.

## Deliverables

1. `voom worker run-local --kind <ffmpeg|mkvtoolnix>` foreground command.
2. `voom policy create` / `voom policy version add` / `voom policy list` /
   `voom policy show` subcommands.
3. A whole-scan input-set builder so a real directory (not just one file) can be
   executed in a single run (see Component B).
4. A committed sample policy (remux to MKV + transcode video to HEVC).
5. An end-to-end integration test over a small real media fixture, plus a
   documented manual runbook for running against `/mnt/pool0/test-video`.
6. A pre-dispatch endpoint liveness check in `compliance execute` plus an
   actionable error when a required worker kind has no live (registered and
   reachable) worker.

## Components

### A. `voom worker run-local --kind <ffmpeg|mkvtoolnix>`

Productizes `TestWorkerLaunch::start`. Behavior:

1. Self-heal: retire any prior live (registered/active) local worker for this
   kind, so a previous hard kill that left a stale endpoint doesn't accumulate.
   **Implementation note (commit `6470a5f`):** `workers.name` is globally `UNIQUE`
   (migration 0002) and retire does not free the name, so a fixed name like
   `local-ffmpeg` cannot be re-registered. The worker is registered with a unique
   name `"<base>-<random>"` (base `local-ffmpeg` / `local-mkvtoolnix`) and
   self-heal matches prior live workers by the base prefix. Safe because runtime
   discovery (`policy_runtime_registry`) selects by operation + status, never by
   name.
2. Register a worker (node-less; see Assumptions) via the existing
   `register_worker`.
3. Generate a random secret. Spawn the bundled worker binary
   (`voom-ffmpeg-worker` or `voom-mkvtoolnix-worker`) located via the existing
   sibling-binary resolution, with `VOOM_WORKER_SECRET`, `VOOM_WORKER_ID`,
   `VOOM_WORKER_EPOCH=0`, and `VOOM_WORKER_BIND=127.0.0.1:0`.
4. Read the worker's `BOUND addr=<socketaddr>` startup line.
5. Record the capability with `extra = {"endpoint": <addr>, "secret": <secret>}`
   and the matching grant (`can_execute`, `max_parallel`) the runtime registry
   and scheduler need. Capability operations per kind:
   - ffmpeg: `transcode_video`, `transcode_audio`, `extract_audio`
   - mkvtoolnix: `remux`
6. Supervise the child in the foreground with an explicit async signal handler
   (`tokio::signal` for SIGINT and SIGTERM). Ctrl-C is the normal operator exit,
   so without an installed handler the default signal disposition would terminate
   `run-local` *before* it can retire the worker — leaving exactly the stale
   endpoint this step exists to prevent. On signal or stdin EOF: close the
   child's stdin (its watchdog shuts down the HTTP server), wait for exit, then
   retire the worker so the endpoint leaves the registry. Graceful retire is
   best-effort (a `kill -9` still skips it); the durable guarantee is the
   step-1 startup self-heal plus the execute-side liveness check (see Error
   handling).
7. Once the capability and grant are recorded (step 5), emit a JSON **readiness**
   line on stdout (`{status:"ready", worker_id, kind, endpoint}`) so the operator
   and the e2e harness have a deterministic safe-to-proceed signal — `execute`
   must not be run until each worker is ready, or it races the registration and
   hits the missing-worker path. Emit a final JSON envelope on clean shutdown.
   Log supervision events to stderr.

To keep its database lock window minimal under the multi-process topology (see
Concurrency and DB access), `run-local` does not hold an open write transaction
during supervision: it registers (steps 1-5), then supervises, then re-acquires
a connection only to retire on exit.

`ffprobe` (initial scan and between-phase re-probe) and `verify-artifact` are
**not** registry workers. The control plane already spawns them as managed
subprocesses from sibling binaries (`scan/worker.rs`, `artifact/worker.rs`),
which works from the CLI today. `run-local` does not cover them.

### B. `voom policy` authoring subcommands

Thin CLI wrappers over existing control-plane methods; no new domain logic.

- `voom policy create --slug <slug> --file <policy.voom>` ->
  `create_policy_document(slug, source)`; emits the created document + accepted
  version id.
- `voom policy version add --document-id <id> --file <policy.voom>` ->
  `add_policy_version(document_id, source)`; emits the new accepted version id.
- `voom policy list` -> `list_policy_documents`.
- `voom policy show --document-id <id>` -> document + `list_policy_versions`.

These extend the existing `PolicyCommand` enum, which currently has only the
`Input` subcommand. The new subcommands sit alongside `Input`.

**Whole-scan input-set builder.** The existing `voom policy input create-from-scan` builds an input
set from a *single* `file_version_id` + `media_snapshot_id` and requires the
operator to hand-type `--container`/`--video-codec`
(`crates/voom-control-plane/src/cases/policy/policy_inputs.rs:13-19`). That makes
the stated "run against a real directory / `/mnt/pool0/test-video`" goal
unreachable: a library scan yields many file-versions. This slice therefore adds
a whole-library mode `voom policy input create-from-scan --all`. There is **no
durable scan id** (scans return an ephemeral report; nothing persists a
`scan_id`), so the anchor is "all live (non-retired) file-versions with a video
media snapshot" rather than a scan id — sufficient for the functional-test flow,
where the DB is dedicated to the scanned library. (`--under <path>` location
filtering is a future refinement, out of scope.) The builder derives each file's
container/video-codec from its persisted media snapshot rather than from CLI
args, producing one multi-file input set. This reuses the existing
`PolicyInputSetDraft { media_snapshots: Vec<_> }` domain support
(`policy_inputs.rs`), so it is a CLI/case-layer addition, not new domain logic.
Single-file `create-from-scan` is retained alongside the new whole-scan mode.

Selection rule: the whole-scan builder includes only file-versions whose latest
media snapshot has a video stream, so non-video and unprobeable files in a real
directory (subtitles, images, `.nfo`, anything `ffprobe` couldn't classify as
video) are skipped rather than producing input rows the video policy can't act
on; the skipped count is reported in the command's JSON output.

### C. Sample policy

Committed as a fixture and referenced by the runbook, two sequential phases —
remux first, then transcode the remuxed result:

```
policy "remux to mkv and transcode to hevc" {
  phase remux {
    container mkv
  }
  phase transcode {
    depends_on: [remux]
    transcode video to hevc
  }
}
```

Phases are barriers across files and chain sequentially (ADR-0007): the transcode
phase operates on the remux phase's committed output, not the original source.
Within a single phase operations are independent, which is why the earlier
single-phase form ran remux and transcode both against the source.

**Observed planner behavior, per phase (verified by the Task 1 oracle test,
`crates/voom-control-plane/tests/sample_policy_plan.rs`; this replaces the earlier
single-phase contract).** The oracle validates per-phase **planning** from a
single `generate_compliance_report` call. That call produces one whole-policy plan
over the input set's *declared* facts; it does NOT run the phase-barrier chain-tip
progression, so the transcode phase is planned against the original source facts,
not against the remux phase's committed mkv output. The runtime chain-tip behavior
(transcode operating on the remuxed output) is covered by the end-to-end test
(Component D), not the oracle. Per `(container, video_codec)` input, planned ops
by phase:

- mp4/h264 → remux `[Remux]`, transcode `[TranscodeVideo]`
- mp4/hevc → remux `[Remux]`, transcode `[TranscodeVideo]`
- mkv/h264 → remux `[]` (already mkv), transcode `[TranscodeVideo]`
- mkv/hevc → remux `[]`, transcode `[]` (already compliant)

Note mp4/hevc still plans a TranscodeVideo even though the source codec is already
hevc, while mkv/hevc does not: because the report plans over declared source
facts, the non-mkv-container input drives a transcode in the same whole-policy
plan, whereas the fully-compliant mkv/hevc input is a complete no-op.

The oracle test pins this per-phase operation set so a planner change that alters
it fails loudly. The end-to-end test (Component D) must assert against this real
behavior, not the earlier assumption.

### D. End-to-end test and runbook

Integration test that exercises the **real operator topology**, not the
in-process shortcut: it spawns the actual `voom worker run-local` binary as
separate child processes for ffmpeg and mkvtoolnix (it does not register workers
in-process via `TestWorkerLaunch`), then runs `compliance execute` as its own
`voom` process against the same on-disk SQLite DB. This is what makes the test
cover the multi-process / DB-concurrency path (see Concurrency and DB access) and
the `run-local` lifecycle. It builds the ffmpeg/mkvtoolnix/ffprobe/verify
workers, generates a small fixture **directory** with an `.mp4`/h264 video file
(which — per §Component C's observed behavior — plans **both** a Remux and a
TranscodeVideo, so a single file exercises both the mkvtoolnix and ffmpeg
workers) plus one non-video file (e.g. `.txt`/`.srt`) that must be skipped by the
whole-scan builder. It **waits for each `run-local` `ready` line** before
proceeding, then runs `voom init` -> `scan <dir>` -> `policy create` ->
`policy input create-from-scan --all` -> `compliance execute`. It asserts
the input set reports `skipped_count == 1`, that execution drives both workers
and commits MKV/HEVC output, and that the report passes. Execution is the oracle
(as in Task 1): the test asserts the *actually committed* artifacts for the
`[Remux, TranscodeVideo]` file rather than a pre-assumed count.
Using a directory (not a single file) is deliberate: it also covers the
whole-scan input-set builder, and the fixture directory includes one non-video
file (e.g. a `.txt`/`.srt`) to assert the builder skips it and reports the skip.

#### Runbook (`/mnt/pool0/test-video`)

For a human operator running against a real library:

1. `voom init` first (`run-local` and every command below open an existing DB via
   `connect`, which never creates or migrates it — ADR-0003).
2. Start `voom worker run-local --kind ffmpeg` and `--kind mkvtoolnix` in their
   own terminals; wait for each to print its `ready` line before step 3. Requires
   `ffmpeg`, `ffprobe`, and `mkvtoolnix` on the host.
3. `scan <dir>` -> `policy create` -> `policy input create-from-scan --all`
   (whole-scan, multi-file) -> `compliance execute` with `--staging-root` /
   `--output-dir`.

The runbook must state:

- **`policy create` is not idempotent (slug is `UNIQUE`).** `policy_documents.slug`
  has a `UNIQUE` constraint (`migrations/0007_policy_registry.sql`), so re-running
  `policy create --slug <s>` on an existing DB errors. On a re-run, either use
  `policy list` to find the existing document id and add a revision with
  `policy version add`, or pick a new slug. The id to pass to `execute` comes from
  the `policy create` / `policy list` JSON.
- **Add-only commit and re-runs.** Commit never overwrites; outputs land under
  the chosen roots. A real library run will partially succeed (some files
  committed, some failed). Re-running `compliance execute` resumes via the
  Sprint 16 per-file-phase resume path (issue-165) rather than redoing committed
  work; document how to read partial state through `compliance report --job-id`
  and what the success signal is for an incremental run.
- **Scale.** Staging needs free disk on the order of the transcoded output set;
  real transcodes are long-running. The automated test uses only a small
  fixture, so these expectations live in the runbook, not the test.
- **Empty / all-non-video scan.** If the scan yields no video files, the
  whole-scan builder produces an empty input set and `compliance execute` is a
  no-op that exits 0 with a "0 files" report — not an error. The operator's
  success signal for such a run is "0 planned, 0 committed."

## Data flow

```
(once) voom init                                     (creates + migrates the DB)

terminal 1: voom worker run-local --kind ffmpeg      (foreground; prints `ready`, then supervises)
terminal 2: voom worker run-local --kind mkvtoolnix  (foreground; prints `ready`, then supervises)
   -> each writes a worker row with extra={endpoint,secret} into the DB

terminal 3 (operator, after both `ready` lines):
   voom scan --path <dir>                  (control plane spawns ffprobe subprocess)
   voom policy create --slug ... --file sample.voom
   voom policy input create-from-scan --all   (whole-scan, video files only)
   voom compliance execute --policy-version-id ... --input-set-id ... \
        --staging-root ... --output-dir ...
       -> reads policy_runtime_registry() from DB
       -> liveness-checks each endpoint, then
       -> dispatches remux -> mkvtoolnix, transcode -> ffmpeg over loopback HTTP
       -> spawns ffprobe/verify subprocesses for re-probe and verification
       -> add-only commit of outputs
```

## Concurrency and DB access

This slice introduces the first genuine multi-process access to one on-disk
SQLite file: two `run-local` processes plus the operator's `execute`, all opening
their own pool against the same DB. The store currently runs **rollback-journal
mode, not WAL**, with a 30s `busy_timeout` (`crates/voom-store/src/pool.rs:40,
46-49`); the code comment defers WAL "until concurrent access pressure." All
existing tests are single-process, so this path has no coverage today.

Stance for this slice (chosen to stay scoped, not to migrate the store):

- `run-local` minimizes its lock window: it holds no open write transaction
  during supervision (registers, then supervises with the pool idle/closed, then
  reopens only to retire). After registration the operator's `execute` is
  effectively the sole writer.
- Implementation prerequisite to verify: `execute` must commit durable state
  before dispatching to a worker and must not hold a write transaction across
  worker I/O (transcodes run for seconds-to-minutes; a write lock held that long
  would exceed `busy_timeout` and fail other processes). If the current
  coordinator violates this, fixing it is a prerequisite of this slice.
- The e2e test (Component D) runs the real three-process topology and asserts it
  completes without `SQLITE_BUSY` / `DbUnreachable` errors.

Store-wide WAL is the durable fix for concurrent readers during a long `execute`
(e.g. a second `voom ... report`/`worker list` while a run is in flight) and is
**recommended for the broader Sprint 17**, but it is a store-level change with
`init`/migration implications and is deferred out of this slice.

## Error handling and lifecycle

- **Stale endpoints.** Graceful `run-local` exit retires the worker, but that
  only fires if the explicit signal handler runs (Component A step 6), and it
  cannot help an `execute` that starts in the window between a hard kill and the
  next `run-local`. So the durable protections are: (a) `run-local` startup
  retires same-name leftovers, and (b) `execute` performs a cheap pre-dispatch
  liveness check on each registry endpoint and drops workers whose endpoint is
  unreachable, *before* dispatching, rather than dispatching blind to a dead
  endpoint at submit time. **Implemented** (commit `f3b9400`): the probe is the
  worker-protocol `handshake` (`ClientHandle::handshake(PROTOCOL_VERSION)`) under
  a 500 ms timeout — a protocol-aware probe, not a bare TCP connect, so a dead
  port or a non-voom listener on a reused ephemeral port is correctly treated as
  unreachable. Exact worker identity (secret/epoch) is still enforced by the
  existing auth-gated `/v1/operations` dispatch, so a reused port held by a
  *different* voom worker fails cleanly at dispatch. **Scope:** an operation whose
  registered worker(s) all fail the probe (the stale-endpoint case) fast-fails
  with the actionable error below, before any issue/ticket write. An operation
  with **no registered worker at all** retains the pre-existing per-ticket
  "no eligible candidate" partial-coverage behavior (so intentionally running a
  subset of workers still dispatches what it can) — see Acceptance.
- **Worker dies mid-run**, including the operator stopping a `run-local` while
  `execute` is running (Component A step 6 tears down the worker's HTTP server
  under any in-flight call). The pre-dispatch liveness check cannot prevent this:
  phases (remux, transcode) and files commit independently and add-only, so a
  death after some commits leaves partial results. This is not corruption — it
  falls back to the Sprint 16 per-file-phase resume path (issue-165); re-running
  `execute` resumes from durable state. The "no partial commit" guarantee holds
  only for the already-unreachable-at-submit case.
- **Worker preflight failure** (no `ffmpeg` / `mkvtoolnix` on host): `run-local`
  exits non-zero with the worker's dependency error before registering anything.
- **`compliance execute` with no live worker of a needed kind**: surface an
  actionable error naming the missing operation and the `run-local` command to
  start it.

## Assumptions

- **Node-less local workers** for this slice, matching `TestWorkerLaunch`. The
  runtime registry (`policy_runtime_registry`) and the execute path do not
  require a node; tests prove node-less capability + grant is sufficient. Proper
  local-node association (`NodeKind::Local`, node-token auth) is deferred.
- Secret stored plaintext in `worker_capabilities.extra`, reached only over
  loopback. This matches the existing tested design.
- `run-local` is foreground and one-per-kind. A "launch all kinds" convenience
  and any backgrounding are out of scope.

## Out of scope

Daemon and background supervision; remote nodes; node-token auth for local
workers; backup worker, sidecar ingest, library-root / scan-config CRUD,
use-lease commands, issue action commands; the full Sprint 17 daemon-readiness
matrix; store-wide WAL migration (recommended for broader Sprint 17, see
Concurrency and DB access); continuous registry health monitoring beyond the
pre-dispatch liveness check.

## Acceptance criteria

- `voom worker run-local --kind ffmpeg` and `--kind mkvtoolnix` each register a
  worker with a live endpoint, supervise the child, and clear the registration on
  graceful exit (verified by inspecting `voom worker list` / the DB before and
  after).
- `voom policy create` from a `.voom` file produces an accepted policy version
  usable by `compliance execute`; `voom policy list` / `show` report it.
- `voom policy input create-from-scan --all` over a multi-file scan
  produces one input set covering every scanned file-version.
- With both `run-local` workers up as separate processes, `voom compliance
  execute` (its own process, same on-disk DB) against a scanned fixture directory
  (an `.mp4`/h264 file that plans both Remux and TranscodeVideo, plus a non-video
  file) drives both the mkvtoolnix and ffmpeg workers, commits MKV/HEVC output,
  and returns a passing compliance report end-to-end, and a concurrent
  `voom worker list` issued during the run succeeds (no `DbUnreachable`) within
  the `busy_timeout` window.
- `compliance execute` with a required operation's only registered worker(s)
  **unreachable at submit time** (the stale-endpoint case) fails fast before any
  dispatch — with an error naming the operation and the `run-local` command to
  start it — leaving no partial commit. An operation with **no registered worker
  at all** retains the existing per-ticket partial-coverage behavior (dispatch
  what you can), so intentionally running a subset of workers is unaffected.
  (Mid-run worker death is handled by Sprint 16 resume, not by this guarantee;
  see Error handling.)

## Verification expectations

- End-to-end CLI integration test (real fixture directory) covering `voom init`
  -> scan -> policy create -> whole-scan input -> execute -> committed outputs +
  report, with `run-local` ffmpeg/mkvtoolnix spawned as **separate `voom`
  processes** and `execute` run as its own process against the same on-disk DB.
  This validates that the real multi-process topology runs without errors. Note:
  because `run-local` holds no transaction during supervision (Component A),
  `execute` is the steady-state sole writer, so this is not a journal-mode
  contention stress test. To get a real concurrency signal, the test also issues
  a concurrent reader (`voom worker list`) while `execute` runs and asserts it
  succeeds within the `busy_timeout` window; deeper contention stress is out of
  scope (mooted by the idle-supervision design and the deferred WAL switch).
- Whole-scan input-set test: `policy input create-from-scan --all` over a
  multi-file scan produces one input set covering all video file-versions with
  container/codec derived from each snapshot (not from CLI args), and **skips
  non-video / unprobeable files**, reporting the skipped count.
- `run-local` lifecycle tests against the real binary: emits the `ready` line
  after registration, endpoint recorded, retirement on graceful signal
  (SIGINT/SIGTERM) and stdin EOF, same-name leftover self-heal on restart.
- Planner-oracle test: the sample policy produces the expected operation set for
  each of the four codec/container input combinations (transcode-only for
  non-HEVC inputs, remux-only for HEVC-non-MKV, no-op for HEVC-MKV) — the
  correctness contract from Component C.
- Liveness tests: `execute` fails fast (no partial commit) when a registered
  worker's endpoint is unreachable at submit time (simulate by killing the worker
  but leaving a stale row) and when no worker of a kind is registered; and a
  reused-port case is not mistaken for a live worker (authenticated ping rejects
  a non-matching listener).
- Policy CLI golden-output tests for create / version add / list / show,
  including the uninitialized-DB error path for `run-local` and policy commands.
- `just ci` green.

## Closeout documentation

A short closeout note recording the commands shipped, the manual runbook for
`/mnt/pool0/test-video`, and the items still owed by the broader Sprint 17
(backup, sidecar, library-root CRUD, use-lease/issue commands, daemon-readiness
matrix).
