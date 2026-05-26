# Issue 90 Policy File Source Resolution Design

## Context

Issue #90 tracks duplicated policy source handling in the workflow executor and
binding layer. `PolicyTranscodeSource` and `PolicyRemuxSource` carry the same
fields, and `resolve_policy_transcode_source` duplicates
`resolve_policy_remux_source` except for the operation name in unsupported-target
errors.

## Success Criteria

- Replace the two binding source structs with one `PolicyFileSource` carrying
  `file_version_id` and optional `location_id`.
- Replace the two executor source resolvers with one
  `resolve_policy_file_source(target, operation_name)` helper.
- Preserve current error semantics:
  - missing file locations return `NotFound("file_location {id}")`;
  - retired file locations return `Config("file_location {id} is retired")`;
  - transcode unsupported target errors include the operation name passed by the
    caller;
  - remux unsupported target errors remain binding errors so CLI messages keep
    the `workflow root payload binding` context.
- Preserve rendered payload shape for transcode and remux tickets.

## Design

Add `PolicyFileSource` in `workflow/binding.rs` and update
`render_policy_transcode_payload` plus `render_policy_remux_payload` to accept
that shared type. No payload field names change.

In `workflow/executor.rs`, replace the transcode/remux-specific source resolver
methods with:

```rust
async fn resolve_policy_file_source(
    &self,
    target: &voom_plan::TargetRef,
    operation_name: &str,
) -> Result<PolicyFileSource, VoomError>
```

The helper matches `FileVersion` and `FileLocation` exactly as the old functions
did. Transcode unsupported targets use `{operation_name} requires file_version or
file_location target, got {other:?}`. Remux keeps its existing binding-level
unsupported-target branch because CLI snapshots depend on the binding context.

## Testing

Add characterization tests for retired transcode and remux file-location targets
before refactoring. Then run:

```bash
cargo test -p voom-control-plane policy_transcode_file_location_target
cargo test -p voom-control-plane policy_remux_file_location_target
cargo test -p voom-control-plane workflow::binding
```

Run `just fmt-check` after editing.
