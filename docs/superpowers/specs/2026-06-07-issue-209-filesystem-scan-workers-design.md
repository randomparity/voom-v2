# Issue 209 Filesystem-Aware Scan Workers Design

## Problem

`voom scan` discovers all candidate media files, launches one bundled ffprobe
worker, and probes each file serially. That preserves conservative disk access
on a single filesystem, but it leaves independent filesystems idle when one scan
root spans multiple mounted devices.

## Goals

- Use one ffprobe worker per filesystem identity when a directory scan discovers
  candidates on multiple filesystems.
- Keep single-filesystem scans on one worker.
- Keep each filesystem group serial so a single disk is not thrashed.
- Preserve report order, summary counters, drift checks, worker attribution, and
  first-fatal-error semantics.
- Test filesystem grouping with injected identities, not host mount layout.
- Ensure every launched worker is shut down on normal completion and scan
  failure after worker launch, with process-drop termination as the cancellation
  backstop.

## Non-Goals

- No CLI flag, config knob, or user-visible concurrency setting.
- No tuning of per-filesystem worker counts beyond one worker per identity.
- No cross-process scheduler or worker pool.
- No changes to the JSON scan envelope contract.

## Design

Add a small scan-local filesystem classifier:

- Production classifier returns a filesystem identity for a candidate path.
- On Unix, the identity is `std::os::unix::fs::MetadataExt::dev()` from
  `tokio::fs::metadata`.
- On non-Unix platforms, all candidates use a single identity.
- Tests pass an in-memory classifier through a private
  `scan_path_with_launcher_and_classifier` helper.

After discovery:

1. Classify every candidate path and keep its original discovery index.
2. Group candidates by filesystem identity, preserving candidate order inside
   each group.
3. Launch one worker per group.
4. Run each group in its own async task. Each task hashes and probes its group
   serially, then shuts its worker down.
5. Group tasks send probe outcomes to the main scan future through a bounded
   channel.
6. The main scan future buffers out-of-order outcomes by original index and
   applies consecutive outcomes in discovery order.

Persistence stays on the main scan future and happens only when the next
original-order outcome is available. That keeps DB side effects and report
construction equivalent to the current serial loop: if candidate 2 has a fatal
error, candidate 3 may already have been probed, but candidate 3 is not
persisted or reported before candidate 2 is applied.

## Outcome Handling

Each group emits one outcome per candidate:

- `Probed`: original candidate, observed file facts, worker id, probe result.
- `WorkerError`: candidate, observed file facts, worker id, scan worker error.
- `ObserveError`: candidate path and discovery scan error.
- `ProbeRequestError`: candidate, observed file facts, worker id, file error.

The ordered applier reuses the existing `ScanReportBuilder` logic:

- `Probed` persists the media snapshot and reports a scanned file.
- Directory-mode `WorkerError` with `is_ffprobe_exit()` records a failed file
  and continues, matching issue #211.
- Other `WorkerError`, `ObserveError`, `ProbeRequestError`, and persist errors
  return the same command error shape as the serial path.

## Failure and Shutdown

All launched group workers are owned by their group task. Each task calls
`shutdown()` after its serial group loop finishes, including when the group
returns early after a worker/observe/request error.

If the ordered applier sees a fatal error, it stops applying later outcomes but
continues receiving group outcomes until every group task finishes, so every
launched worker reaches its explicit `shutdown()` path. This may perform extra
probe work after the first fatal candidate, but it avoids leaking worker
processes and does not persist later results.

Group tasks are joined by the scan future rather than detached. If the outer
scan future is cancelled before those tasks finish, task cancellation drops the
owned worker sessions. The production worker process already uses
`kill_on_drop(true)` and `BundledWorkerProcess::drop` starts killing unreaped
children, so cancellation terminates rather than leaves long-lived workers.

## Tests

Add behavior tests in `crates/voom-control-plane/src/scan/mod_test.rs`:

- Single filesystem directory scan launches one worker, dispatches every probe
  on that worker, and shuts it down.
- Multi-filesystem directory scan launches one worker per injected filesystem
  identity, dispatches each candidate on that filesystem's worker, preserves
  report file order, and shuts all workers down.
- A fatal probe error in one group preserves first-fatal report semantics and
  still shuts all launched workers down.

Existing scan tests continue to verify snapshots, sidecars, drift protection,
unprobeable-file continuation, explicit-file fatal behavior, and non-UTF-8
request failures.
