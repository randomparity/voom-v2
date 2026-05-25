# Issue 71 Policy Input From Scan Design

## Context

Issue #71 is still open. The codebase has durable policy input-set storage and a
control-plane `create_policy_input_set` use case, but the CLI cannot create an
input set. Chaos Librarian tests still call Rust test support to seed policy
input rows directly after `voom scan`.

## Decision

Add a public CLI path:

```bash
voom policy input create-from-scan \
  --slug chaos-h264 \
  --file-version-id 1 \
  --media-snapshot-id 1 \
  --container mp4 \
  --video-codec h264
```

This is intentionally narrower than a scan-envelope importer. The command
creates exactly one `MediaSnapshotInput` from existing durable scan rows and
explicit normalized policy facts. It does not create policy documents, parse
scan JSON files, infer canonical containers from ffprobe names, add an HTTP API
route, or support multi-file input sets.

## Control-Plane API

Add a typed control-plane method:

```rust
create_policy_input_set_from_scan(input: PolicyInputFromScanInput)
```

The method validates inside one transaction that:

- `file_version_id` exists and is not retired;
- `media_snapshot_id` exists;
- the media snapshot belongs to the supplied file version.

Missing IDs return `NOT_FOUND`. Retired file versions or mismatched
file/snapshot IDs return `CONFLICT`. Validation or insertion errors leave no
partial policy input rows because the method composes validation and
`create_input_set_in_tx` inside one transaction.

## Policy Input Shape

The created input set uses:

- `source_kind = imported`;
- `display_name = slug`;
- one generated fixture label, `scan-<slug>`, to satisfy the existing policy
  input-set model invariant;
- one media snapshot with target `FileVersion { id }`;
- explicit `container` and `video_codec` from CLI flags;
- `stream_summary = {"video_stream_count": 1}`;
- `existing_media_snapshot_id = media_snapshot_id`;
- no synthetic targets, evidence, bundle targets, quality profiles, or issues.

Width, height, bitrate, duration, audio languages, and subtitle languages remain
unset in this issue. The current transcode compliance path only needs known
container, video codec, file-version target, and the existing snapshot link.

## CLI Contract

Add nested CLI commands under `policy input` to keep room for future policy and
input-set management commands:

```bash
voom policy input create-from-scan ...
```

The command emits exactly one JSON envelope on stdout with command `"policy"`.
On success, `data` contains:

```json
{
  "input_set": {
    "input_set_id": 1,
    "slug": "chaos-h264",
    "source_kind": "imported",
    "file_version_id": 1,
    "media_snapshot_id": 1
  }
}
```

Runtime errors use the existing envelope error mapping and exit code `2`.
Argument parsing errors continue to use the existing `BAD_ARGS` envelope path
and exit code `1`.

## Tests

Add control-plane tests for successful creation, missing IDs, retired file
versions, and snapshot/file-version mismatch. Add CLI integration tests that
scan the tiny media fixture, invoke the public command, and then run `voom plan
show` with the returned input-set id. Add a negative CLI test for missing scan
rows to pin stable error envelopes.

## Review Notes

Adversarial review concern: the command still requires `--container` and
`--video-codec`, so it is not a full scan-envelope-to-policy importer. That is
intentional for this issue: deriving canonical policy facts from ffprobe
container aliases would add domain normalization decisions that are not needed
to unblock Chaos/local policy execution and are better handled by a later
importer.
