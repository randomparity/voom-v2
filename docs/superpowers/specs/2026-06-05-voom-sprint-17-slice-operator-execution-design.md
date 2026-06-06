# Sprint 17 Slice: Operator-Runnable Real-Media Execution

Date: 2026-06-05
Status: Design approved, pending spec review
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
3. A committed sample policy (remux to MKV + transcode video to HEVC).
4. An end-to-end integration test over a small real media fixture, plus a
   documented manual runbook for running against `/mnt/pool0/test-video`.
5. An actionable error from `compliance execute` when a required worker kind has
   no live registration.

## Components

### A. `voom worker run-local --kind <ffmpeg|mkvtoolnix>`

Productizes `TestWorkerLaunch::start`. Behavior:

1. Retire any prior live (registered/active) local worker with the same derived
   name (a stable per-kind name, e.g. `local-ffmpeg`, `local-mkvtoolnix`), to
   self-heal a previous hard kill that left a stale endpoint.
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
6. Supervise the child in the foreground. On SIGINT / stdin EOF: close the
   child's stdin (its watchdog shuts down the HTTP server), wait for exit, then
   retire the worker so the stale endpoint is removed from the registry.
7. Emit a single JSON envelope on a clean shutdown; log supervision events to
   stderr.

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

### C. Sample policy

Committed as a fixture and referenced by the runbook. Two phases, to exercise
the Sprint 16 artifact-chaining and between-phase re-probe path:

```
policy "remux to mkv and transcode to hevc" {
  phase remux     { container mkv }
  phase transcode { depends_on: [remux]
                    transcode video to hevc }
}
```

The compliance model already treats files that are already MKV / already HEVC as
compliant, so "transcode only if not already HEVC" is inherent in planning; no
explicit `where` guard is required.

### D. End-to-end test and runbook

Integration test (mirrors `multi_phase_flow.rs` but exercises both remux and
transcode): build the ffmpeg/mkvtoolnix/ffprobe/verify workers, start
`run-local` for ffmpeg and mkvtoolnix, generate a small non-HEVC non-MKV
fixture, then drive the real `voom` binary: `scan` -> `policy create` ->
`policy input create-from-scan` -> `compliance execute`, and assert committed
MKV/HEVC outputs and the compliance report.

A short runbook documents the same sequence for a human running against
`/mnt/pool0/test-video`, including the requirement that `ffmpeg`, `ffprobe`, and
`mkvtoolnix` are installed on the host and that outputs are add-only under the
chosen `--staging-root` / `--output-dir`.

## Data flow

```
terminal 1: voom worker run-local --kind ffmpeg      (foreground, supervises)
terminal 2: voom worker run-local --kind mkvtoolnix  (foreground, supervises)
   -> each writes a worker row with extra={endpoint,secret} into the DB

terminal 3 (operator):
   voom scan --path <dir>                  (control plane spawns ffprobe subprocess)
   voom policy create --slug ... --file sample.voom
   voom policy input create-from-scan ...
   voom compliance execute --policy-version-id ... --input-set-id ... \
        --staging-root ... --output-dir ...
       -> reads policy_runtime_registry() from DB
       -> dispatches remux -> mkvtoolnix, transcode -> ffmpeg over loopback HTTP
       -> spawns ffprobe/verify subprocesses for re-probe and verification
       -> add-only commit of outputs
```

## Error handling and lifecycle

- **Stale endpoints** are the primary risk. Graceful `run-local` exit retires the
  worker; `run-local` startup retires same-name leftovers. A hard `kill -9`
  between runs can still orphan a row; this is a documented limitation, cleared
  by re-running `run-local`. A registry health-probe in `execute` is deferred.
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
matrix; registry endpoint health-probing.

## Acceptance criteria

- `voom worker run-local --kind ffmpeg` and `--kind mkvtoolnix` each register a
  worker with a live endpoint, supervise the child, and clear the registration on
  graceful exit (verified by inspecting `voom worker list` / the DB before and
  after).
- `voom policy create` from a `.voom` file produces an accepted policy version
  usable by `compliance execute`; `voom policy list` / `show` report it.
- With both `run-local` workers up, `voom compliance execute` against a scanned
  non-MKV non-HEVC fixture commits an MKV/HEVC output and a passing compliance
  report, end-to-end through the real `voom` binary.
- `compliance execute` with a required worker kind absent fails with an error
  naming the missing operation and the `run-local` command to start it.

## Verification expectations

- End-to-end CLI integration test (real fixture media) covering scan ->
  policy create -> input -> execute -> committed output + report.
- `run-local` lifecycle tests: registration on start, endpoint recorded,
  retirement on graceful exit, same-name leftover self-heal on restart.
- Policy CLI golden-output tests for create / version add / list / show.
- Actionable-missing-worker error test.
- `just ci` green.

## Closeout documentation

A short closeout note recording the commands shipped, the manual runbook for
`/mnt/pool0/test-video`, and the items still owed by the broader Sprint 17
(backup, sidecar, library-root CRUD, use-lease/issue commands, daemon-readiness
matrix).
