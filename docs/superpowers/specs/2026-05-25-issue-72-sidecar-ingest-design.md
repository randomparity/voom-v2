# Issue 72 Sidecar Ingest Design

## Goal

Promote the Chaos observed-state sidecar workaround into product scan behavior.
A directory scan that finds a supported media file and matching `.srt` subtitle
sidecars must persist the sidecars as durable file identity rows, link them to
the primary media through existing bundle tables, and expose enough public scan
output for callers and test exporters to derive the relationship from durable
state.

## Current State

`voom scan` discovers only media extensions. Directory `.srt` files are reported
as skipped unsupported entries and are never persisted. The Chaos observed-state
exporter compensates by scanning the filesystem near each media path and
emitting nested `sidecars[]` from filename heuristics in test support. Existing
schema tables already model the needed durable relationship:

- `media_works`
- `media_variants`
- `asset_bundles`
- `asset_bundle_members`
- `file_assets`, `file_versions`, `file_locations`

No migration is required for the first `.srt` implementation.

## Chosen Approach

Directory scan will keep media files as the primary scan candidates and will
discover matching sidecars as metadata attached to those candidates. Explicit
file scan remains media-only.

For each successfully persisted media file:

1. Discover same-directory `.srt` files whose stem is either exactly the media
   stem or starts with `<media-stem>.`.
2. Observe each sidecar's size and `sha256:` content hash. Media scan continues
   to use its existing worker-facing `blake3:` hash; sidecars use `sha256:`
   because the Chaos observed-state contract and fixtures compare sidecar
   content with SHA-256.
3. Persist each sidecar with `record_discovered_file_in_tx`, producing normal
   file asset/version/location rows without ffprobe or media snapshot rows.
4. Ensure the primary media asset belongs to a bundle. If the primary asset is
   already a bundle member, reuse that bundle. Otherwise create a provisional
   `media_work`, `media_variant`, and `asset_bundle`, then add the primary asset
   with role `primary_video`.
5. Add each sidecar asset to the same bundle with role `external_subtitle`.

This is intentionally provisional identity. It does not attempt title parsing,
series matching, alternate variants, language extraction, or cross-directory
sidecar search. Those are later library-management problems.

## Public Surface

`ScanFileReport` and the CLI envelope will gain optional bundle fields for
successful primary media rows:

- `bundle_id`
- `sidecars[]`

Each sidecar entry contains:

- `path`
- `file_asset_id`
- `file_version_id`
- `file_location_id`
- `content_hash`
- `size_bytes`
- `bundle_id`
- `bundle_member_role = "external_subtitle"`

The primary media row also reports `bundle_member_role = "primary_video"` when
the scan created or reused a bundle link.

This satisfies inspection for the first implementation without adding a new CLI
subcommand. A future bundle/sidecar inspect command can reuse the same durable
rows.

## Matching Rules

Only same-directory sidecars match in this cycle. Matching is case-insensitive
for the `.srt` extension and case-sensitive for stems. Given primary
`Movie.Name.mkv`, the following are sidecars:

- `Movie.Name.srt`
- `Movie.Name.eng.srt`
- `Movie.Name.forced.srt`

The following are not sidecars:

- `Movie.srt`
- `Movie.Name.nfo`
- `Other.Name.eng.srt`
- sidecars in the scan root but not next to the primary media

When multiple media files could match one sidecar, deterministic assignment uses
the longest matching media stem, then lexicographic media path as tie-breaker.
That prevents `Movie.srt` from stealing `Movie.Part1.eng.srt` when
`Movie.Part1.mkv` exists.

## Error Handling and Atomicity

Media, sidecar, bundle, and bundle-member writes for one primary candidate are
one transaction. A media file is only marked `scanned` after the primary media
identity rows, media snapshot, matched sidecar identity rows, and bundle links
all commit. If any matched sidecar cannot be observed or persisted, that primary
file fails with a normal scan error and no partial rows are committed for that
file. Already committed earlier files in the directory remain committed,
matching current scan behavior.

Unsupported non-matching files are still reported as skipped unsupported
entries. Matched `.srt` sidecars are not reported as skipped.

Duplicate scans should be idempotent enough for local workflows:

- an already-member primary asset reuses its bundle;
- an already-member sidecar in the same bundle is treated as already linked;
- an already-member sidecar in a different bundle is a conflict and fails loud.

Because scan identity is currently provisional and has no physical proof row,
repeated scans may create a fresh file asset and provisional bundle instead of
finding the previous scan's asset. The required behavior in that case is still
no membership conflict and no partial write. When identity resolution returns an
already-member asset, the reuse rules above apply.

The scan summary counts matched sidecars as discovered and ingested. Matched
sidecars do not increment probed or snapshots-recorded because they are not sent
to ffprobe and do not produce media snapshots.

## Observed-State Export

The Chaos test exporter will stop discovering `.srt` files from the filesystem.
It will query `asset_bundle_members` for sidecar assets linked to the primary
asset's bundle and emit nested `sidecars[]` from durable rows. Sidecar
`content_hash` comes from the sidecar file version's `sha256:` durable hash, so
the exporter can compare Chaos sidecar bytes without re-reading the filesystem.
Assets whose only bundle membership role is `external_subtitle` are not emitted
as top-level observed assets; they appear only inside the primary asset's
`sidecars[]`.

## Tests

Focused tests will cover:

- discovery attaches `.srt` sidecars to media candidates and no longer skips
  matched sidecars;
- control-plane scan persists a media file plus `Movie.eng.srt` as two file
  assets, one media snapshot, one provisional work/variant/bundle, and two
  bundle members with the expected roles;
- repeated scan links sidecars without duplicate membership failure, including
  the current no-proof identity case that creates a fresh provisional bundle;
- CLI scan envelope includes the durable sidecar IDs;
- Chaos observed-state export derives `sidecars[]` from bundle membership rather
  than filesystem probing.

## Out of Scope

- Embedded subtitle extraction or modification.
- `.nfo`, poster, trailer, commentary, transcript, and audio sidecars.
- Language parsing from sidecar filenames.
- User-facing bundle inspect commands beyond scan envelope fields.
- Cross-directory matching or library-root fallback matching.
- Schema migrations or long-term canonical media identity matching.
