# Issue 211: Unprobeable Directory Scan Design

## Goal

Allow a directory scan to finish when one supported media file cannot be probed by
ffprobe, while preserving fatal behavior for infrastructure, drift, database, and explicit
single-file failures.

## Current Behavior

`ControlPlane::scan_path_with_launcher` returns `ScanCommandError` for any
`dispatch_probe_file` error. The CLI then emits `status: error` and exits `2`, even when
the failure is a single corrupt or malformed media file inside a larger directory.

The ffprobe worker currently reports invalid media as a terminal worker error with
`ExternalSystemUnavailable` and a structured payload containing `"stage": "exit"`. The
same error class also covers missing ffprobe binaries and spawn failures with other
payload stages, so the control plane must not continue on all `ExternalSystemUnavailable`
errors.

## Design

Preserve terminal worker error payloads through `WorkerStreamError::Terminal` and
`ScanWorkerError`. Add a narrow `ScanWorkerError::is_ffprobe_exit()` predicate that
recognizes `ExternalSystemUnavailable` terminal errors whose payload has
`{"stage": "exit"}`. Directory scans will treat only that predicate as a per-file
unprobeable-media failure. For those files, the scan report records a failed file entry
with observed hash/size, worker id, error code/class/message, increments `summary.failed`,
and continues to the next candidate.

All other failures keep current behavior:

- explicit file scan with ffprobe exit remains `status: error`;
- directory scan content drift remains `status: error`;
- missing `ffprobe`, spawn errors, worker crashes, timeouts, malformed protocol results,
  and database/persist errors remain `status: error`;
- no media snapshot or file identity rows are persisted for the unprobeable file.

`summary.probed` continues to count successful probe results only; failed ffprobe exits
increase `summary.failed` but not `summary.probed`.

## Policy Input Behavior

`policy input create-from-scan --all` already builds from live file versions with latest
video snapshots and skips versions without video snapshots. Since unprobeable files do
not create snapshots, they remain excluded. Existing included/skipped count behavior
continues to apply to durable rows; there is no durable failed-scan row to count.

## Testing

Add a control-plane directory scan test with two supported files: one fake-success file
and one fake ffprobe-exit file. Assert the scan returns `Ok`, records one snapshot,
reports one scanned file and one failed file, and shuts down the worker.

Add a paired explicit-file test for the same ffprobe-exit failure to prove single-file
scans still return an error.

Add a test that missing/spawn-style worker failures still abort a directory scan so the
continuation rule cannot accidentally swallow infrastructure failures.

The CLI contract is covered through the control-plane result shape and existing scan
envelope serialization: an `Ok(ScanReport)` yields one JSON envelope with `status: ok`.

## Scope

This issue does not add a new public error code, new durable scan-failure table, or new
policy input mode. It preserves an existing worker error payload internally and changes
directory scan control flow only for ffprobe process exits.
