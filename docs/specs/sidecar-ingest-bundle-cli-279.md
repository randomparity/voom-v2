# Sidecar ingest for V1 asset types + bundle CLI (#279)

Part of #269, Workstream C. Depends on: —. Blocks: T20 (#289).

## Problem

Bundles and members are real and populated by scan
(`crates/voom-control-plane/src/scan/persist.rs`), but two gaps remain:

1. **Discovery matches `.srt` only.** `is_supported_sidecar_path`
   (`scan/discovery.rs`) accepts a single extension, and every discovered
   sidecar is persisted with a hardcoded `external_subtitle` role
   (`scan/persist.rs`). `.nfo`, poster/fanart artwork, and trailers exist only
   as `BundleMemberRole` enum values and DB `CHECK` vocabulary — nothing emits
   them.
2. **No bundle CLI.** Bundle state is visible only inside `voom scan` JSON
   output. There is no way to list bundles or inspect one bundle's members,
   roles, and lineage.

The V1 external asset set is defined in
`docs/specs/voom-control-plane-design.md` (Asset Bundle Model): primary video,
external audio, external subtitle, poster/fanart, NFO/metadata, trailer,
transcript, generated thumbnail, policy report.

## Goal

- Scan ingests the V1 external sidecar set (`.srt`, `.nfo`, poster/fanart
  images, trailers) and registers each as a bundle member under the correct
  role.
- Operators can run `voom bundle list` and `voom bundle show --bundle-id <id>`
  to inspect bundles, their members and roles, and the media work/variant
  lineage the bundle belongs to.

Non-goals (V1): folder-level artwork with no stem match (`poster.jpg`,
`fanart.jpg`, `folder.jpg`); finer artwork roles (fanart/banner/thumbnail
distinct from poster); subtitle extensions beyond `.srt`; a bundle-member
provenance column. These are captured as follow-ups.

## Design

### Sidecar classification

Discovery gains a pure classifier `classify_sidecar(path) -> Option<SidecarKind>`
keyed on the file's lowercased extension and stem. `SidecarKind` is a scan-local
enum (keeps `discovery.rs` free of a `voom-store` dependency); `persist.rs` maps
it to `BundleMemberRole`.

| Input (lowercased) | `SidecarKind` | `BundleMemberRole` |
|---|---|---|
| ext `srt` | `Subtitle` | `external_subtitle` |
| ext `nfo` | `Nfo` | `nfo` |
| ext `jpg` `jpeg` `png` `webp` `tbn` | `Poster` | `poster` |
| ext in `SUPPORTED_EXTENSIONS` **and** stem ends `-trailer` or `.trailer` | `Trailer` | `trailer` |
| otherwise | `None` (skipped) | — |

`SidecarCandidate` gains a `kind: SidecarKind` field alongside `path`.

### Discovery flow

In `discover_directory`, each regular file is classified before the
primary-media check, because trailers carry a media extension:

1. `classify_sidecar(path)` is `Some(kind)` → sidecar candidate with `kind`.
2. else `is_supported_media_path(path)` → primary candidate.
3. else → skipped (`UnsupportedExtension`).

Ordering is safe: `classify_sidecar` returns `Trailer` only for a media-extension
file whose stem carries the trailer suffix, and `None` for an ordinary media
file (which then becomes a primary candidate).

### Sidecar → media matching

`sidecar_matches_media` currently attaches a sidecar to a media file when the
sidecar stem equals the media stem or extends it after a `.` separator. Extend
the separator set to also accept `-`, so `Movie-poster.jpg`, `Movie-fanart.jpg`,
and `Movie-trailer.mkv` attach to `Movie.mkv`, and `Movie.en.srt` continues to.
The existing longest-matching-stem tie-break is unchanged.

An orphan sidecar (no matching media in the directory) is reported in `skipped`
exactly as an orphan `.srt` is today — no silent drop.

### Persistence

`ObservedSidecar` and `PersistedSidecar` carry the resolved `BundleMemberRole`.
`persist_sidecar` uses that role for both the idempotent re-scan check
(`member.role == expected_role`) and the `add_member` insert, replacing the
hardcoded `external_subtitle`. The DB `CHECK` already admits every role
(migration `0013`), so no migration is required. Scan JSON output surfaces the
per-member role through the existing `bundle_member_role` field.

### `voom bundle` CLI

New top-level `Command::Bundle(BundleCommand)` with `List` and `Show`
subcommands, following the standard envelope conventions (one JSON envelope on
stdout; `voom_core::ErrorCode` codes; `NOT_FOUND` exit 2 for a missing bundle).

- `voom bundle list [--limit N]` — all bundles ordered by id, each summarised
  as `{id, media_variant_id, display_name, created_at, member_count}`. The
  member count is produced by a single aggregate query
  (`LEFT JOIN ... GROUP BY bundle_id`), not one `list_members` call per bundle,
  so the list view is a single round trip regardless of bundle count.
- `voom bundle show --bundle-id <id>` — one bundle with:
  - `bundle`: `{id, media_variant_id, display_name, created_at, epoch}`
  - `lineage`: the `media_variant` (`{id, media_work_id, label, provisional}`)
    and `media_work` (`{id, kind, display_title, provisional}`) it belongs to
  - `members`: each `{file_asset_id, role, file_version_id, content_hash,
    size_bytes, produced_by, produced_from_version_id, location}`, ordered by
    member id. **File-version selection is deterministic:** the member's
    provenance is read from its single live (`retired_at IS NULL`) file version;
    if more than one is live, the highest `id` wins; if none is live, the
    `file_version_id`, `content_hash`, `size_bytes`, `produced_by`,
    `produced_from_version_id`, and `location` fields are `null`. `location` is
    the live local-path location of that chosen version, highest `id` if
    several, else `null`. Reading provenance from `file_versions` lets generated
    members (extracted audio) show their `produced_from_version_id` without a
    new schema column.

New read accessors (all thin, no events): `SqliteBundleRepo::list_all(limit)`
returning `(AssetBundle, member_count)` rows from the aggregate query, plus
`ControlPlane` wrappers for it and for the identity reads the view needs
(`get_media_variant`, `get_media_work`, `list_file_versions_by_asset`,
`list_live_file_locations_by_version`) where not already exposed.

## Acceptance criteria

- `.srt`, `.nfo`, poster/fanart image, and `-trailer` media files in a scanned
  directory attach to the matching media candidate with roles
  `external_subtitle`, `nfo`, `poster`, `trailer` respectively.
- An orphan sidecar of any supported kind appears in `skipped`, not as a
  member.
- Within one scan, a sidecar whose `file_asset` is already a member of the
  bundle under the same role is not double-added — the `persist_sidecar` guard
  (`get_member_by_file_asset_in_tx`) returns the existing membership — and a
  conflicting role for an existing member is rejected with a `Conflict`. This is
  the existing subtitle guard, now applied per role. Cross-scan / whole-library
  re-scan identity dedup is **not** provided here: `ingest_new_file_asset`
  mints a fresh `file_asset` per discovered file (identity never collapses on
  content hash, and `file_locations` has no `UNIQUE(value)`), so a second scan
  of the same directory creates new identities today. That is a pre-existing
  property of the ingest layer, out of scope for #279, and not claimed as an
  acceptance criterion here.
- `voom bundle list` emits one envelope listing every bundle with a member
  count; `voom bundle show` emits members with roles, lineage, and per-member
  provenance; a missing id yields a `NOT_FOUND` envelope with exit 2.
- `just ci` is green; new insta snapshots for the bundle commands are reviewed
  and committed.

See ADR [0022](../adr/0022-sidecar-role-classification.md).
