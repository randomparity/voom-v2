# Issue 88 Remux Failure Context Design

## Context

Issue #88 tracks repeated failure-event calls in
`crates/voom-control-plane/src/remux/mod.rs`. `execute_remux_core` is
intentionally linear, but each failure branch repeats the current
`source_location_id`, optional selection, staging path, remux result, and staged
artifact IDs.

## Success Criteria

- Introduce a small local `RemuxFailureContext` for `execute_remux_core`.
- Preserve the linear workflow and all current fallible step ordering.
- Preserve failure event payloads at each stage, including:
  - no source location when source selection fails;
  - source location after source selection;
  - selection and staging path only after those facts are known;
  - result only after worker result is available;
  - staged artifact IDs only after staging is recorded.
- Remove the repeated long optional argument lists from `execute_remux_core`.

## Design

Add a private `RemuxFailureContext<'a>` near the existing failure recording
helpers. It stores:

- `cp: &'a ControlPlane`
- `input: &'a ExecuteRemuxInput`
- `source_location_id: Option<FileLocationId>`
- `selection: Option<&'a RemuxSelection>`
- `staging_path: Option<&'a Path>`
- `result: Option<&'a RemuxResult>`
- `staged: Option<&'a commit::StagedRemuxArtifact>`

The context exposes setter-style methods that return updated contexts and an
async `record_failure(&self, err)` method that calls `events::record_failed`.
`execute_remux_core` creates the context at the start and shadows it as new facts
become available.

## Testing

This is a behavior-preserving refactor. Existing remux tests already check
partial failure, started/failed event counts, staged artifact IDs on verification
failure, and post-commit recovery behavior. Run:

```bash
cargo test -p voom-control-plane remux
cargo test -p voom-cli --test compliance_envelope execute_scanned_remux_existing_target_outputs_failure_envelope
just fmt-check
```
