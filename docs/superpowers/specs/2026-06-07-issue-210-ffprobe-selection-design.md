# Issue 210: Ffprobe Selection Design

## Goal

Prevent real `voom scan` runs from silently selecting a sibling `ffprobe` test helper in
`target/debug`, while preserving explicit fake-ffprobe opt-in for tests and operators.

## Current Behavior

`BundledWorkerProcess::launch_bundled_ffprobe` resolves the worker binary from
`VOOM_FFPROBE_WORKER_BIN`, a sibling `voom-ffprobe-worker`, or `PATH`. During command
construction, `bundled_ffprobe_command_from` also checks the resolved worker directory for
a sibling `ffprobe` binary. If found, it injects `VOOM_FFPROBE_BIN=<worker_dir>/ffprobe`
into the worker process environment.

That implicit injection is unsafe for development builds because `target/debug/ffprobe`
can be a canned test helper. The worker then reports successful scans using fake metadata.

## Design

Remove implicit sibling `ffprobe` selection from production scan worker command
construction. The scan launcher will still resolve and start `voom-ffprobe-worker` the
same way, but it will not add `VOOM_FFPROBE_BIN` unless the parent process already has
that environment variable set.

The ffprobe worker already defaults to `ffprobe` when `VOOM_FFPROBE_BIN` is absent, so a
development `target/debug/voom` scan uses the system ffprobe discoverable on `PATH`
instead of `target/debug/ffprobe`. Tests that require fake ffprobe continue to opt in by
setting `VOOM_FFPROBE_BIN` explicitly on the CLI command or worker command.

## Error Handling and Observability

If an operator sets `VOOM_FFPROBE_BIN`, `voom scan` emits an envelope warning naming the
selected binary path. This satisfies the operator-facing explicitness requirement without
changing scan report data. The selected binary also remains visible in persisted snapshot
metadata because the ffprobe worker records `provider_version` from that binary.

If `ffprobe` is missing from `PATH`, the existing worker-domain terminal error remains the
failure mode.

## Testing

Update scan worker command tests so the default command no longer injects a sibling
`ffprobe`, even when one exists beside the worker. Keep explicit fake-ffprobe tests using
`VOOM_FFPROBE_BIN`.

Add or adjust an integration-level scan test so a CLI run without `VOOM_FFPROBE_WORKER_BIN`
still finds the sibling worker while fake ffprobe remains explicitly supplied only through
`VOOM_FFPROBE_BIN`.

Add a CLI serialization test or integration assertion that `VOOM_FFPROBE_BIN` produces a
scan envelope warning naming the configured ffprobe path.

## Scope

This issue does not change ffprobe worker protocol payloads, scan report JSON shape, or
policy input behavior. It does add an envelope warning for explicit ffprobe overrides, but
does not add operator configuration flags; environment variables remain the explicit
override mechanism.
