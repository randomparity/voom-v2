---
status: accepted
date: 2026-07-02
deciders: [VOOM core]
---

# 0022 — Stem-prefix sidecar classification for V1 asset ingest

## Context

Scan discovery attaches sidecar files to a media candidate and persists them as
bundle members, but only for `.srt` subtitles, always under the
`external_subtitle` role (`scan/discovery.rs`, `scan/persist.rs`). The V1 asset
set (`docs/specs/voom-control-plane-design.md`, Asset Bundle Model) also
includes NFO/metadata, poster/fanart artwork, and trailers, whose
`BundleMemberRole` values and DB `CHECK` vocabulary already exist but are never
emitted by scan (#279).

Two facts constrain the design:

- **The existing matching rule is stem-based.** A sidecar attaches to the media
  file whose stem it shares or extends (`Movie.en.srt` → `Movie.mkv`), with a
  longest-matching-stem tie-break. Any new asset type has to slot into that
  model or replace it.
- **Trailers carry a media extension.** `Movie-trailer.mkv` matches
  `SUPPORTED_EXTENSIONS`, so without intervention it is discovered as its own
  primary media candidate rather than as a sidecar of `Movie.mkv`.

## Decision

Classify sidecars by extension and stem suffix, keeping the existing
stem-prefix matching model.

- **A pure `classify_sidecar(path) -> Option<SidecarKind>`** keyed on lowercased
  extension: `srt` → subtitle, `nfo` → NFO, `jpg`/`jpeg`/`png`/`webp`/`tbn` →
  poster, and a media-extension file whose stem ends `-trailer`/`.trailer` →
  trailer. `persist.rs` maps `SidecarKind` to `BundleMemberRole`
  (`external_subtitle`, `nfo`, `poster`, `trailer`).

- **Classification runs before the primary-media check** in `discover_directory`,
  so a trailer-suffixed media file is routed to sidecars rather than becoming a
  primary candidate, while an ordinary media file (classifier returns `None`)
  still becomes a candidate.

- **Matching accepts `-` as well as `.` as the stem separator**, so
  `Movie-poster.jpg` and `Movie-trailer.mkv` attach to `Movie.mkv`. Orphans
  (no matching media) are reported in `skipped`, never silently dropped.

- **All artwork maps to `poster`.** The role enum has no distinct fanart/banner
  value; fanart images register as `poster` in V1.

## Consequences

- No migration: `asset_bundle_members.role` `CHECK` already admits every role
  (migration `0013`). Scan JSON output surfaces the per-member role through the
  existing `bundle_member_role` field.
- Per-file-named assets (`Movie-poster.jpg`, `Movie.nfo`, `Movie-trailer.mkv`)
  are ingested; folder-level artwork with no stem match (`poster.jpg`,
  `fanart.jpg`) is not, and remains in `skipped`.
- A media file deliberately named `*-trailer` with no base sibling is skipped
  rather than ingested as primary media. This is visible in the scan report;
  the `-trailer` suffix is treated as an explicit sidecar signal.
- The classifier is a single choke point, so adding a subtitle extension or a
  distinct fanart role later is a localized change.

## Considered & rejected

- **Directory-scoped folder artwork** (`poster.jpg`/`fanart.jpg` attached to the
  sole media file in a directory). More useful for real Kodi/Plex libraries but
  needs a different, directory-level matching model with an ambiguity rule for
  multi-media directories. Deferred to a follow-up; out of V1 scope.
- **Distinct fanart/banner/thumbnail roles by suffix.** Speculative refinement
  with no consumer yet; all artwork maps to `poster` until a consumer needs the
  distinction.
- **A bundle-member provenance column.** Generated-member provenance already
  lives on `file_versions` (`produced_by`, `produced_from_version_id`); the
  bundle CLI surfaces it via reads. A dedicated column would be a migration and
  a payload-contract change for no new information. Deferred.
- **Extending subtitle extensions (`.ass`, `.sub`, `.vtt`).** Not requested by
  #279; the classifier makes it a one-line addition when needed.
