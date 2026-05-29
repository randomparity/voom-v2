---
name: voom-sprint-15-design
description: Sprint 15 design for named, validated, durable video encode profiles (HEVC and AV1) referenced by policy or specified inline, applied end-to-end through the planner, worker protocol, FFmpeg workers, and CLI inspection.
status: draft
date: 2026-05-28
sprint: 15
references:
  - docs/specs/voom-control-plane-design.md
  - docs/superpowers/specs/2026-05-25-voom-sprint-12-design.md
  - docs/superpowers/specs/2026-05-25-voom-sprint-13-design.md
  - docs/superpowers/specs/2026-05-26-voom-sprint-14-design.md
---

# VOOM Sprint 15 - Video Profile Settings And Quality Profiles

## 1. Goal

Sprint 15 replaces Sprint 12's single hardcoded HEVC profile with named,
validated, durable video encode profiles. Policies reference a curated built-in
profile by name or specify the encode components inline. The profile model and
DSL are codec-generic, and Sprint 15 implements two target codecs end-to-end:
HEVC and AV1, using software encoders. The FFmpeg worker applies the full field
set per encoder, muxes MKV or MP4 output, downscales to dimension caps, and
stream-copies the video when a source already conforms. The planner uses
dimension-, pixel-format-, and container-aware compliance and surfaces resource
and quality estimates from the resolved profile.

The roadmap title pairs "Video Profile Settings" with "Quality Profiles". The
`QualityScoringProfile` registry (named scoring profiles and `QualityScore`
records) is a separate subsystem that feeds retention, which is not part of the
pre-daemon roadmap. Sprint 15 delivers the named video *encode* profiles and
their planner resource/quality estimates only. The scoring registry stays
deferred.

## 2. Scope

Sprint 15 delivers:

- DSL generalization of `transcode video to <codec>` to accept `hevc` and
  `av1`. Bare `transcode video to hevc` still resolves to the `default-hevc`
  built-in with the exact Sprint 12 command behavior.
- A durable `video_profiles` table carrying `target_codec`, encoder, and the
  full encode field set, seeded by migration with a curated built-in set for
  both codecs. The table is read-only this sprint: no create/update surface.
- Two software encoders for HEVC and three encoder bindings total: HEVC ->
  `libx265`; AV1 -> `libsvtav1` or `libaom-av1`. The encoder field is validated
  against a per-codec allowlist, and the model is structured so more
  encoders/codecs can be added later without reshaping it.
- Per-encoder validation. The finite vocabulary for CRF, preset, tune, codec
  profile, codec level, and pixel format is encoder-specific (for example
  `libx265` CRF 0-51 with named presets `ultrafast..placebo`; `libsvtav1` CRF
  0-63 with numeric `-preset 0-13`; `libaom-av1` CRF 0-63 with `-cpu-used 0-8`
  and required `-b:v 0`). Validation is keyed on the resolved encoder.
- A compiler that emits a typed `VideoProfileRef` that is either `Named` or
  `Inline`, fully validating inline settings and rejecting unknown encoders,
  codec/encoder mismatches, out-of-range values, malformed values, incompatible
  combinations, and a policy that supplies both `using profile` and an inline
  body.
- A control-plane resolution step that turns `Named` references into a typed
  profile from the registry, rejecting unknown names with a stable diagnostic
  before planning.
- Planner compliance that is dimension-, pixel-format-, and container-aware, and
  `ResourceEstimates` populated deterministically from the resolved profile.
- An ffprobe-normalizer and snapshot-to-planning-input projection extension that
  captures and carries video stream `pixel_format`, `profile`, and `level`, and
  exposes per-stream `kind`+`codec_name` to the planner (needed for
  profile/level/pixel-format compliance and MP4 stream-compatibility detection).
- MKV and MP4 output containers. A profile selects `mkv` or `mp4`. MP4 output is
  supported for sources whose non-video streams MP4 can carry; sources with
  MP4-incompatible non-video streams block with a precise diagnostic.
- A `copy_compatible` profile setting: when the output container changes but the
  source video stream already conforms to the profile codec, pixel format,
  codec profile/level, and dimension caps, the control plane decides the source
  may be stream-copied and the worker copies the video instead of re-encoding.
  The control plane is the copy/encode decision authority; the worker validates
  the decision against the source rather than deciding independently.
- A worker protocol `TranscodeVideoProfile` extended to all encode fields plus
  `target_codec` and `encoder`, and a `TranscodeVideoResult` extended with
  observed output width/height/pixel-format and a `copied_video` flag. The
  bundled FFmpeg worker builds codec/encoder-specific command shapes, applies
  max-dimension downscaling and the `copy_compatible` short-circuit, muxes MKV or
  MP4, and validates output codec/container/dimensions/pixel-format via ffprobe.
- Worker preflight that validates the specific encoder a resolved profile needs
  (`libx265`, `libsvtav1`, or `libaom-av1`); a missing encoder is a loud setup
  failure, not a skipped test.
- Dispatch that consumes the resolved profile instead of the hardcoded
  `default_hevc()` profile.
- CLI `voom profile list` and `voom profile show <name>` inspection commands,
  and transcode execution reports extended with resolved profile facts.
- Sprint 15 closeout evidence tying profile model, validation, resolution,
  planning, payload generation, worker application, and operator inspection to
  repeatable tests.

Sprint 15 explicitly does not deliver:

- The `QualityScoringProfile` registry, `QualityScore` records, or any quality
  scoring computation.
- User-defined profile create/update. The `video_profiles` table is seeded and
  read-only this sprint.
- mov_text subtitle transcoding or attachment handling for MP4 output.
  MP4-incompatible non-video streams block instead.
- Hardware-accelerated encoders (NVENC, QSV, AV1 hardware) and tuning databases.
- Additional software encoders or codecs beyond `libx265`/`libsvtav1`/
  `libaom-av1` and HEVC/AV1 (no `x264`, H.264, VP9, `librav1e`).
- Fractional CRF, bitrate or codec ladders, adaptive outputs, and per-title
  automatic profile selection.
- Plugin-defined encode schemas and free-form FFmpeg argument strings in policy
  text.
- Audio or subtitle profile settings.
- Replace/delete/archive semantics, backup policy, daemon scheduling, remote
  media transfer, object storage, or UI controls.

## 3. Architecture

The Sprint 15 real path extends the Sprint 12 video transcode path:

```text
voom scan --path <file>
  -> FileVersion + FileLocation + MediaSnapshot

accepted policy with `transcode video to <hevc|av1> [using profile "<name>"|{...}]`
  -> compiled TranscodeVideo operation carrying a typed VideoProfileRef
  -> control plane resolves Named refs against video_profiles (unknown -> diagnostic)
  -> planner evaluates compliance against the resolved profile
  -> ExecutionPlan node operation_kind = "transcode_video"
  -> compliance execute submits durable workflow ticket carrying the resolved profile
  -> scheduler leases ticket to builtin.ffmpeg
  -> FFmpeg worker writes a staged MKV or MP4 artifact (re-encode or video copy)
  -> control plane records artifact_handle + staging artifact_location
  -> verify_artifact worker verifies staged bytes
  -> host commit creates add-only FileVersion + FileLocation
  -> scan/probe records MediaSnapshot for committed result
```

The FFmpeg worker never writes SQLite and never commits managed media state. Its
only filesystem mutation is writing the requested staging path. The control
plane owns profile resolution, artifact identity, verification, final commit,
result snapshot persistence, lineage, events, and reports.

The compiled `profile` string from Sprint 12 is already threaded through the
planner node payload and `render_policy_transcode_payload` ticket payload, but
`dispatch::request_for` currently drops it and hardcodes
`TranscodeVideoProfile::default_hevc()`. Sprint 15 carries the resolved profile
through to dispatch and the worker.

### Profile resolution boundary

Profile name resolution happens at planning-input assembly in the control plane,
which already loads the compiled policy and snapshots and has the store. The
compiler stays a pure text-to-model transform and validates only inline
settings. The planner stays pure and consumes a fully-typed profile. This keeps
a single resolution point, keeps name-existence rejection before any planning
work, and matches the Sprint 14 pattern where the compiler does typed shape and
the control plane does the rest.

A `Named` reference is not frozen into the compiled policy; the compiled policy
stores the name and resolution happens per plan. Built-in profiles are read-only
this sprint, so resolution is deterministic across runs.

## 4. Data Model

A new migration `0014_video_profiles.sql` creates the registry, seeded with
built-ins:

```sql
CREATE TABLE video_profiles (
  id              TEXT PRIMARY KEY,          -- stable UID
  name            TEXT NOT NULL UNIQUE,      -- "default-hevc"
  target_codec    TEXT NOT NULL,             -- "hevc" | "av1"
  encoder         TEXT NOT NULL,             -- "libx265" | "libsvtav1" | "libaom-av1"
  crf             INTEGER NOT NULL,
  preset          TEXT NOT NULL,             -- encoder-specific domain: "medium" | "8"
  tune            TEXT,                       -- nullable
  codec_profile   TEXT,                       -- nullable; the H.265/AV1 profile, e.g. "main10"
  codec_level     TEXT,                       -- nullable; e.g. "5.1"
  pixel_format    TEXT,                       -- nullable
  max_width       INTEGER,                    -- nullable
  max_height      INTEGER,                    -- nullable
  output_container TEXT NOT NULL DEFAULT 'mkv', -- "mkv" | "mp4"
  copy_compatible INTEGER NOT NULL DEFAULT 0,
  CHECK (length(trim(name)) > 0),
  CHECK (target_codec IN ('hevc', 'av1')),
  CHECK (encoder IN ('libx265', 'libsvtav1', 'libaom-av1')),
  CHECK (crf >= 0),
  CHECK (max_width IS NULL OR max_width > 0),
  CHECK (max_height IS NULL OR max_height > 0),
  CHECK (output_container IN ('mkv', 'mp4')),
  CHECK (copy_compatible IN (0, 1))
) STRICT;
```

The table is `STRICT` with `CHECK` constraints matching the repo's schema
convention (12 of 13 existing migrations are `STRICT`); the enum/range checks
enforce profile validity at the DB layer rather than relying solely on app code,
so a malformed seed row or future writer cannot persist a profile the resolver
would turn into an invalid worker request. A per-encoder CRF *range* depends on
the encoder and stays an app-level validation; the DB check only bounds CRF to
non-negative.

The V1 table omits ownership/audit columns (`is_builtin`, `created_at`,
`updated_at`): every row is migration-seeded and read-only this sprint, so those
columns would serve only the deferred user-CRUD feature. The sprint that adds
profile create/update introduces them in its own migration.

The architecture's `profile`/`level` profile fields are stored as
`codec_profile`/`codec_level` to avoid colliding with the profile record itself.
CRF is integer in V1 (`libx265` 0-51, AV1 encoders 0-63); fractional CRF is
deferred. The `preset` column holds an encoder-specific token: a named x265
preset, a numeric SVT-AV1 `-preset`, or a numeric libaom `-cpu-used` value.

### Seeded built-ins

| name | codec | encoder | crf | preset | container | notable |
|------|-------|---------|-----|--------|-----------|---------|
| `default-hevc` | hevc | libx265 | 23 | medium | mkv | unchanged from Sprint 12 (all optional fields unset) |
| `hevc-archive` | hevc | libx265 | 18 | slow | mkv | `codec_profile=main10`, `pixel_format=yuv420p10le` |
| `hevc-1080p` | hevc | libx265 | 23 | medium | mp4 | `max 1920x1080`, `copy_compatible=true`, exercises MP4 |
| `default-av1` | av1 | libsvtav1 | 30 | 8 | mkv | SVT-AV1 defaults |
| `av1-archive` | av1 | libaom-av1 | 20 | 4 | mkv | exercises libaom `-cpu-used` + `-b:v 0` |
| `av1-1080p` | av1 | libsvtav1 | 32 | 8 | mp4 | `max 1920x1080`, `copy_compatible=true` |

`default-hevc` keeps every new field unset so the resolved
`TranscodeVideoProfile` produces the same FFmpeg command line as Sprint 12
(omitted optional flags). The JSON serialization is not byte-identical: the
struct gains keys (see the serde policy in Typed models below), so Sprint 12
transcode request/result fixtures are updated as part of this sprint.

### Typed models and crate placement

- `voom-worker-protocol`: extend `TranscodeVideoProfile` to carry
  `target_codec`, `encoder`, `crf`, `preset`, optional `tune`, `codec_profile`,
  `codec_level`, `pixel_format`, `max_width`, `max_height`, and
  `copy_compatible`. `default_hevc()` keeps the new fields unset/false. New
  optional fields use `#[serde(skip_serializing_if = "Option::is_none")]` and
  `copy_compatible` skips when false, so a `default_hevc()` payload serializes
  to a minimal superset of the Sprint 12 shape and the command line is
  unchanged; existing fixtures still gain the newly required `target_codec`
  key (`name`, `encoder`, `crf`, `preset` already exist), so Sprint 12 golden
  fixtures are updated. This
  crate also homes the per-encoder capability descriptors as pure data and
  predicates: for each encoder, the CRF range, preset domain (named-set vs
  numeric-range), tune set, codec-profile set, codec-level set, pixel-format
  set, and ffmpeg flag-mapping rules. This crate already owns the transcode
  codec constants and is a shared dependency of policy, plan, and worker.
- `voom-policy`: `VideoProfileRef` (`Named(String)` | `Inline(VideoProfileSettings)`);
  the compiler calls the capability predicates to emit policy diagnostics for
  invalid inline settings. `CompiledOperation::TranscodeVideo` changes its
  `profile: String` field to `profile: VideoProfileRef`, and `target_codec`
  accepts `hevc` or `av1`. `VideoProfileRef` must deserialize the legacy
  bare-string form (see Compiled-Policy Compatibility below).
- `voom-store` / `voom-control-plane`: the `video_profiles` repository
  (lookup-by-name, list) and the resolution step that turns a `Named` reference
  into a typed `TranscodeVideoProfile`.
- `voom-ffprobe-worker` (+ snapshot projection): extend the normalizer to
  capture video `pixel_format`, `profile`, and `level`, and carry per-stream
  `kind`+`codec_name` through the snapshot-to-planning-input projection.

### Compiled-Policy Compatibility

`CompiledOperation::TranscodeVideo.profile` is a durably persisted field.
Compiled policies are stored as serialized JSON in `policy_versions.compiled_json`
and deserialized into `voom_policy::CompiledPolicy` at planning/compliance time
(`crates/voom-control-plane/src/cases/compliance.rs`). Every accepted policy
from Sprints 12-14 stored `"profile": "default-hevc"` as a bare JSON string.
Changing the field type to a tagged `VideoProfileRef` would otherwise fail to
deserialize those existing rows and break planning for already-accepted
policies.

Sprint 15 keeps backward compatibility at the serde layer rather than migrating
stored rows:

- `VideoProfileRef` deserializes a bare JSON string `"name"` as
  `Named("name")`, in addition to its tagged `Named`/`Inline` forms. A custom
  or `untagged` deserializer handles the legacy shape.
- New compiled policies serialize the tagged form; legacy rows continue to read
  back as `Named`.
- Because the on-read representation is backward compatible, the
  `policy_versions.schema_version` value does not need to change and existing
  `compiled_json` rows are not rewritten. If implementation finds a serde
  approach that cannot round-trip the legacy string without a schema bump, it
  must bump `schema_version` and recompile-on-read instead, never silently drop
  the profile.
- A regression test asserts that a `compiled_json` document containing
  `"profile": "default-hevc"` (bare string) still deserializes and plans to the
  `default-hevc` profile.

## 5. Policy And Planning

### Grammar

```text
transcode video to hevc                                  # -> Named("default-hevc")
transcode video to av1                                   # -> Named("default-av1")
transcode video to hevc using profile "hevc-archive"     # -> Named("hevc-archive")
transcode video to av1 {                                 # -> Inline(VideoProfileSettings)
  encoder: libsvtav1
  crf: 28
  preset: 6
  pixel_format: yuv420p10le
  max_width: 3840
  max_height: 2160
  output_container: mp4
  copy_compatible: true
}
```

`using profile "<name>"` and an inline body are mutually exclusive; supplying
both is a policy validation error. Inline settings are typed and finite and are
never free-form FFmpeg arguments.

The inline body is a list of `key: value` settings reusing the existing
`SettingAst` form already used by `metadata`/`config` blocks (so the parser
needs no new statement syntax). Its contract:

- `encoder`, `crf`, and `preset` are mandatory in an inline body. `encoder` is
  required because all other validation is keyed on the resolved encoder, and
  AV1 has two encoders (`libsvtav1`, `libaom-av1`) with different CRF/preset
  domains, so there is no unambiguous default; an inline body without `encoder`
  is a validation error. (The bare `transcode video to <codec>` form, which
  carries no inline body, still resolves to the named `default-<codec>` built-in
  whose encoder is fixed.)
- `tune`, `codec_profile`, `codec_level`, `pixel_format`, `max_width`,
  `max_height`, `output_container`, and `copy_compatible` are optional; an
  omitted optional key means unconstrained (or, for `output_container`, `mkv`,
  and for `copy_compatible`, false).
- Unknown keys and duplicate keys are validation errors with a stable
  diagnostic. The body does not silently ignore or last-write-wins.

### Compiler validation

The compiler validates inline settings against the per-encoder capability
descriptors and rejects:

- unknown encoder names;
- encoder/target-codec mismatch (for example `libx265` under `to av1`);
- CRF outside the encoder's range;
- preset outside the encoder's domain (named vs numeric);
- unknown tune, codec_profile, codec_level, or pixel_format for the encoder;
- incompatible combinations such as a 10-bit pixel format under an 8-bit-only
  codec profile;
- a missing mandatory inline key (`encoder`, `crf`, or `preset`);
- an unknown or duplicate inline key;
- `using profile` combined with an inline body.

`Named` references are passed through by the compiler. The seeded registry is
pre-validated, and name existence is checked at resolution.

### Resolution

When assembling the planner input, the control plane resolves a `Named`
reference by looking it up in `video_profiles`. An unknown name is a stable
diagnostic, and no planning occurs. The result is a fully-typed
`TranscodeVideoProfile` that flows into both the planner and the ticket payload.
A resolved `Named` profile keeps its registry `name`; a resolved `Inline`
profile is assigned a synthetic, deterministic `name` of `inline-<hash>`, so
every resolved profile has a stable identity for the ticket payload, reports,
and target-path naming. `<hash>` is computed over a **canonical, version-stable**
representation of the resolved settings — the fields serialized in a fixed
order with normalized values (lowercased codec/encoder/pixel-format tokens,
integer CRF, trimmed preset token, absent optionals omitted) — not raw serde
output, so the hash does not drift when the struct's serde layout changes. It is
at least the first 12 hex characters of a BLAKE3 digest of that canonical form,
chosen long enough that two distinct inline profiles do not collide in practice.
Tests assert the hash is identical across a serde round-trip and differs for two
near-identical profiles (for example CRF 22 vs 23).

Resolution is shared by the executing and non-executing plan paths. The
plan-only path (`plan_compiled_policy_with_input` / a dry-run `voom plan`)
resolves named profiles against the same registry read and applies the same
dimension/pixel-format/profile/level/container-aware compliance, so unknown-name
rejection and no-op/planned/blocked decisions are identical whether a plan is
produced for inspection or for execution. There is no execute-only resolution
path.

### Planner compliance

Planner compliance uses only observable snapshot facts. A `transcode_video` node
is:

- No-op when, for the single video stream, every *observable* constraint the
  profile sets is already satisfied: container equals `profile.output_container`,
  codec equals `target_codec`, the source is within `max_width`/`max_height`
  when dimensions are constrained, the source pixel format matches when
  constrained, and the source codec profile/level match when `codec_profile`/
  `codec_level` are constrained.
- Planned when the source is fully identified but not compliant: wrong codec,
  too wide/tall, wrong pixel format, wrong codec profile/level, or a container
  change.
- Blocked (insufficient facts) when a constrained observable fact is unknown:
  container, codec, video stream count, dimensions when dimensions are
  constrained, pixel format when constrained, or codec profile/level when those
  are constrained.
- Blocked (unsupported shape) when there is not exactly one video stream.
- Blocked (unsupported shape) when `output_container` is `mp4` and the snapshot
  enumerates a non-video stream MP4 cannot carry (for example SRT/ASS/PGS
  subtitles or font attachments). The diagnostic names the offending stream(s).
  mov_text subtitle transcoding and attachment handling are deferred.
- Blocked (insufficient facts) when `output_container` is `mp4` and the snapshot
  does not reliably enumerate every non-video stream's type and codec. The
  planner must not pass an unverified source to the worker as the MP4
  compatibility gate; an under-described source blocks rather than proceeding.

`codec_profile`, `codec_level`, and `pixel_format` are treated as observable:
ffprobe reports the bitstream profile, level, and pixel format. However, the
Sprint 10 ffprobe normalizer (`crates/voom-ffprobe-worker/src/normalize.rs`)
does **not** currently record them — it normalizes `index`, `kind`,
`codec_name`, `width`, `height`, `duration`, `avg_frame_rate`, `sample_rate`,
`channels`, `language`, and `disposition` (the raw ffprobe JSON is retained
under `raw.ffprobe_json`, but the planner consumes normalized facts, not the raw
blob). Sprint 15 therefore extends the normalizer to capture video stream
`pixel_format`, `profile`, and `level`, and extends the snapshot-to-planning
projection (and the `policy_media_snapshot_inputs` representation) to carry them
to the planner. This normalizer/projection extension is a Sprint 15 deliverable,
listed in scope and testing. When a profile constrains profile/level/pixel-format
but a snapshot predates the extension (the fields are absent), the node blocks as
insufficient facts rather than silently no-op'ing a non-conforming source.

By contrast, the encoder-internal knobs (CRF, preset, tune) are not recoverable
from a probe of an already-encoded file. They are applied on encode but never
drive a compliance decision.

MP4 compatibility is decided from the `MediaSnapshot` stream inventory: the
planner is the authoritative gate and uses each non-video stream's type and
codec. The V1 MP4-muxable allowlist is HEVC/AV1 video and AAC/AC3/E-AC3/Opus
audio (Opus is included because it pairs commonly with AV1 and modern MP4 muxes
it); text subtitles (SRT/ASS), image subtitles (PGS), and font/other
attachments are not muxable and block the node. Detection reads each stream's
`kind` and `codec_name` from the normalized snapshot — both already captured by
the Sprint 10 normalizer — not the `audio_languages`/`subtitle_languages`
projections (which carry languages, not codecs); the projection must therefore
expose per-stream `kind`+`codec_name` to the planner (via `stream_summary` or an
equivalent), which the normalizer/projection extension above guarantees. The
snapshot-dependency and insufficient-facts rules make a partially-described
source block rather than silently relying on the worker. The worker remains a
loud backstop that fails if a mux is rejected, but it is not the compatibility
decision-maker.

### Resource and quality estimates

`ResourceEstimates.notes` is populated deterministically from the resolved
profile as fixed key-prefixed strings so tests can assert exact content, not
just presence:

- `encoder=<encoder>` and `speed=<preset-or-cpu-used-token>`;
- `cpu_cost=<low|medium|high>` derived from a fixed encoder+speed lookup (for
  example `libx265 placebo` or `libaom-av1 -cpu-used 0` -> `high`);
- `crf=<n>` as the quality/size knob;
- `downscale=<srcWxsrcH>-><capWxcapH>` only when the profile's dimension caps
  will shrink the source.

The notes are human-readable but format-stable; V1 does not estimate wall-clock
time or output size. Tests assert the exact emitted note set for representative
profiles.

## 6. Worker Protocol

`TranscodeVideoProfile` carries the encode knobs: `name`, `target_codec`,
`encoder`, `crf`, `preset`, optional `tune`, `codec_profile`, `codec_level`,
`pixel_format`, `max_width`, `max_height`, and `copy_compatible`. The output
container and codec ride the existing `TranscodeVideoOutput` (`container` now
`mkv|mp4`, `video_codec` now `hevc|av1`). The request also carries a top-level
`copy_video` boolean, decided by the control plane (see the `copy_compatible`
command rule below); `copy_compatible` on the profile is the *eligibility*
setting, `copy_video` is the *decision* for this execution.

Request shape:

```json
{
  "input": {
    "path": "/library/input.mkv",
    "expected": {
      "size_bytes": 1234,
      "content_hash": "blake3:...",
      "modified_at": "2026-05-28T00:00:00Z",
      "local_file_key": null
    }
  },
  "output": {
    "staging_root": "/tmp/voom-stage",
    "path": "/tmp/voom-stage/ticket-1/lease-1/input.av1.mp4",
    "container": "mp4",
    "video_codec": "av1",
    "overwrite": false
  },
  "profile": {
    "name": "av1-1080p",
    "target_codec": "av1",
    "encoder": "libsvtav1",
    "crf": 32,
    "preset": "8",
    "max_width": 1920,
    "max_height": 1080,
    "copy_compatible": true
  },
  "copy_video": false
}
```

Result shape:

```json
{
  "status": "transcoded",
  "provider": "ffmpeg",
  "provider_version": "ffmpeg version ...",
  "input_pre": { "size_bytes": 1234, "content_hash": "blake3:..." },
  "input_post": { "size_bytes": 1234, "content_hash": "blake3:..." },
  "output": { "size_bytes": 987, "content_hash": "blake3:..." },
  "output_container": "mp4",
  "output_video_codec": "av1",
  "output_width": 1920,
  "output_height": 1080,
  "output_pixel_format": "yuv420p",
  "copied_video": false
}
```

`default_hevc()` keeps every new optional field unset/false. With
`skip_serializing_if`, its payload omits the optional keys and the resulting
FFmpeg command line is unchanged from Sprint 12; the serialization still gains
the now-required `target_codec` key, so Sprint 12 request/result fixtures are
updated rather than preserved verbatim.

### Per-encoder command shapes

Commands are built only from typed fields, never from free-form arguments:

- `libx265`: `-c:v libx265 -crf N -preset <named>` plus optional `-tune`,
  `-profile:v`, `-level`, `-pix_fmt`.
- `libsvtav1`: `-c:v libsvtav1 -crf N -preset <0-13>` plus optional `-profile:v`,
  `-pix_fmt`, and tune/level via `-svtav1-params`.
- `libaom-av1`: `-c:v libaom-av1 -crf N -b:v 0 -cpu-used <0-8>` plus optional
  `-tune`, `-profile:v`, `-pix_fmt`. The `-b:v 0` is required for constant-quality
  CRF mode.

Cross-cutting command rules:

- Max-dimension scaling is aspect-preserving and downscale-only; it never
  upscales and enforces even output dimensions. It is applied only when the
  source exceeds a constraint.
- MP4 muxing tags HEVC as `-tag:v hvc1`; AV1 muxes as `av01`.
- `copy_compatible`: the **control plane** decides whether the video may be
  stream-copied, from the authoritative snapshot, and sets the request's
  `copy_video` flag. It sets `copy_video: true` only when the profile is
  `copy_compatible`, the node is planned for a non-video reason (a container
  change), and the source video stream already satisfies the target codec,
  dimension caps, constrained pixel format, and constrained codec profile/level.
  When `copy_video` is true the worker emits `-c:v copy`, validates that its own
  input probe still satisfies those preconditions (failing loudly on
  disagreement rather than copying a non-conforming stream), and sets
  `copied_video: true`. When `copy_video` is false the worker re-encodes. The
  worker never derives the copy decision on its own.
- Non-video streams are copied as in Sprint 12 (`-c:a copy`, and so on). The
  planner has already blocked MP4-incompatible sources; the worker still fails
  loudly if a mux is rejected.

### Worker obligations

The worker must:

- reject unknown operations through the protocol route policy;
- reject malformed payloads before invoking FFmpeg;
- reject an existing output path because Sprint 15 has no overwrite semantics;
- reject missing or non-canonical `output.staging_root` values and reject output
  paths whose canonical parent is outside that root;
- observe and verify input bytes before and after FFmpeg;
- invoke FFmpeg out of process with the deterministic per-encoder command shape
  derived from typed request fields;
- when `copy_video` is true, validate via its input probe that the source video
  stream still satisfies the codec, dimension, pixel-format, and codec
  profile/level preconditions before emitting `-c:v copy`, and fail loudly if it
  does not; never derive the copy decision independently of the request flag;
- emit progress frames when FFmpeg exposes useful progress;
- observe output bytes after FFmpeg exits;
- validate output codec, container, resolution (<= dimension caps), and pixel
  format (when constrained) via ffprobe before returning success;
- fail loudly for content drift, unavailable input/output, spawn/exit failures,
  timeout, malformed output facts, unsupported provider output, and path escape
  attempts.

### Preflight

Worker startup or first use validates that the specific encoder the resolved
profile needs (`libx265`, `libsvtav1`, or `libaom-av1`) is present in the FFmpeg
build, recording resolved binary paths, provider versions, and encoder
availability in worker metadata or the failure payload. A missing encoder is a
loud setup failure with explicit diagnostics, not a skipped test.

## 7. Control-Plane Execution

Compliance execution extends the policy bridge so planned `transcode_video`
nodes submit real workflow tickets carrying the resolved profile. For each
transcode ticket, the control plane must:

1. Parse the workflow ticket payload, source identity, and resolved profile.
2. Require an existing, unretired source `FileVersion`.
3. Require exactly one live local source `FileLocation`, unless the payload
   carries a specific source location ID.
4. Re-read the source media snapshot and re-apply the Sprint 15 compliance
   preconditions against the resolved profile: known container, exactly one
   video stream, known codec, known dimensions when the profile constrains
   dimensions, known pixel format when the profile constrains it, known codec
   profile/level when the profile constrains them, and no MP4-incompatible
   non-video streams when targeting MP4.
5. Re-observe source bytes and compare them to the source version facts before
   dispatch.
6. Compute the `copy_video` decision from the re-read snapshot: true only when
   the profile is `copy_compatible` and the source video stream already
   satisfies the target codec, dimension caps, constrained pixel format, and
   constrained codec profile/level (a container-only change); false otherwise.
7. Choose a canonical new staging path under the configured or command-scoped
   staging directory, including the workflow ticket ID and lease identity.
8. Dispatch `TranscodeVideoRequest` with the resolved profile and the computed
   `copy_video` flag to the bundled FFmpeg worker.
9. Reject worker success if input pre/post facts drift, output facts are
   missing, the worker copied video when `copy_video` was false (or vice
   versa), or output codec/container/dimensions/pixel-format do not satisfy the
   request.
10. Record a staged artifact handle linked to the source `FileVersion`, with one
    live `artifact_locations.kind = 'staging'` row.
11. Verify the staged artifact through the Sprint 11 verification path.
12. Commit the verified staged artifact to an add-only target path.
13. Probe the committed result through the durable scan/probe path and record a
    `MediaSnapshot` for the result `FileVersion`.
14. Record lineage and events.

Target paths are add-only and deterministic, derived from the source stem, the
resolved profile identity, the target codec, and the output container extension
— for example `<source-stem>.<profile-id>.hevc.mkv` or
`<source-stem>.<profile-id>.av1.mp4`. The profile identity is the named
profile's `name`, or for an inline profile a short stable hash of the resolved
settings (`inline-<hash>`); it is sanitized for filename use. Including the
profile identity lets distinct-quality outputs of the same codec and container
coexist instead of colliding on a codec-only name (Sprint 12 had a single
profile and did not need this). If the target still exists — the same profile
already produced this output — the operation fails with `CONFIG_INVALID`;
replace semantics remain deferred.

The control plane applies the same local path hardening as Sprint 12/13:
canonicalize source, staging, and target paths; reject symlink traversal; and
store canonical path values in durable records and CLI output.

## 8. Events And Reporting

Sprint 15 adds no new event types. It extends the existing transcode event
payloads:

- `artifact.transcode_started`
- `artifact.transcode_progress`
- `artifact.transcode_succeeded`
- `artifact.transcode_failed`

Each payload gains the resolved profile name, encoder, target codec, and output
container. Success payloads also include `copied_video` and observed output
dimensions and pixel format. These events are audit facts only; artifact
handles, artifact locations, verification rows, commit records, file versions,
file locations, jobs, tickets, and leases remain the source of truth.

Every payload must include enough correlation data to reconstruct the ticket
attempt without reading provider logs: job ID, ticket ID, lease or attempt
identity, source file version/location IDs, staging path or staged artifact IDs
when known, provider name/version when known, and failure class/public error
code on failure.

CLI reports for transcode execution expose stable IDs for policy version and
input set, plan and report, job and ticket, source file version/location, staged
artifact handle/location, verification row, commit record, result file
version/location, and committed-result media snapshot, plus the resolved profile
name, encoder, target codec, output container, `copied_video`, and observed
output dimensions/pixel format. The command output continues to emit exactly one
JSON envelope on stdout.

CLI profile inspection:

- `voom profile list` emits one JSON envelope containing the seeded profiles
  with name, target codec, encoder, CRF, preset, output container, and the
  remaining fields.
- `voom profile show <name>` emits the full resolved profile; an unknown name
  returns `NOT_FOUND`.

## 9. Error Handling

Stable Sprint 15 behavior:

- Invalid inline profile settings (unknown encoder, codec/encoder mismatch, CRF
  out of range, preset outside the encoder domain, unknown
  tune/codec_profile/codec_level/pixel_format, incompatible combination, or
  `using profile` combined with an inline body): policy validation error at
  compile.
- Unknown named profile at resolution: planning diagnostic reported as
  `CONFIG_INVALID` before execution.
- MP4 target with MP4-incompatible non-video streams: planning or execution
  diagnostic reported as `CONFIG_INVALID` (unsupported media shape), naming the
  offending stream(s).
- Missing source file version or location: `NOT_FOUND`.
- Ambiguous source location: `CONFIG_INVALID`.
- Missing source bytes: `ARTIFACT_UNAVAILABLE`.
- Source drift before or during worker execution: `ARTIFACT_CHECKSUM_MISMATCH`.
- Existing staging or target path: `CONFIG_INVALID`.
- Unsupported media shape (no video stream or multiple video streams): planning
  or execution diagnostic reported as `CONFIG_INVALID` at execution time.
- Missing required encoder at preflight: worker preflight failure reported as
  `EXTERNAL_SYSTEM_UNAVAILABLE` with explicit diagnostics; required tests are
  not skipped.
- FFmpeg spawn/exit failure: `EXTERNAL_SYSTEM_UNAVAILABLE`.
- Output codec/container/dimensions/pixel-format mismatch vs request:
  `MalformedWorkerResult`.
- Worker crash, timeout, malformed result, and protocol errors use the existing
  worker failure taxonomy.
- Output fails verification or commit preconditions:
  `ARTIFACT_CHECKSUM_MISMATCH` or `CONFIG_INVALID` as appropriate.
- Commit failure after filesystem promotion begins must preserve Sprint 11
  `recovery_required` visibility.
- Result media snapshot probe failure after commit must not hide the committed
  result; the error envelope includes result `FileVersion`, `FileLocation`, and
  commit record IDs so an agent can inspect or re-probe.

Silent skips are not allowed. If the control plane records partial durable
state, the error envelope must include enough IDs for an agent to inspect it.

## 10. Testing

Required tests:

- Profile repository and migration tests: every seeded built-in is present and
  valid against its encoder descriptor; lookup-by-name; unknown name returns
  none; an insert violating a `CHECK` (bad `target_codec`/`encoder`/
  `output_container`, negative `crf`) is rejected by the `STRICT` table.
- Per-encoder capability descriptor tests: accepted and rejected CRF ranges,
  preset domains (named vs numeric), tune/codec_profile/codec_level/pixel_format
  vocabularies, and incompatible combinations.
- ffprobe normalizer + projection tests proving video `pixel_format`, `profile`,
  and `level` are captured and reach the planner input, and that per-stream
  `kind`+`codec_name` are exposed for MP4 detection; a pre-extension snapshot
  lacking these fields is treated as insufficient facts.
- Policy parser/validator/compiler tests: named reference; valid inline; each
  inline rejection class (missing mandatory `encoder`/`crf`/`preset`, unknown
  key, duplicate key, `to av1` inline without `encoder`); mutual-exclusion
  error; `to hevc` and `to av1` targets.
- Resolution tests: named reference resolves to settings; unknown name yields a
  diagnostic before planning; inline settings pass through unchanged.
- Dry-run parity tests: the plan-only path rejects an unknown profile name and
  marks a too-wide / wrong-profile source as `planned`, matching the execute
  path.
- Inline-profile identity tests: the `inline-<hash>` is stable across a serde
  round-trip of the resolved settings and differs for two near-identical
  profiles (for example CRF 22 vs 23).
- Planner tests: no-op when all observable criteria match; planned for wrong
  codec, too-wide/tall, wrong pixel format, wrong codec profile/level, and
  container change; blocked-insufficient-facts for unknown
  dimensions/pixel-format/codec-profile/codec-level when constrained;
  blocked-unsupported-shape for not exactly one video stream; blocked for MP4
  target with MP4-incompatible non-video streams; blocked-insufficient-facts for
  MP4 target with an under-enumerated stream inventory; resource estimate notes.
- Worker protocol serialization tests for the extended profile and result
  fields, including proof that a `default_hevc()` request serializes to the
  expected minimal shape (optional keys omitted via `skip_serializing_if`) and
  that its generated FFmpeg command line is unchanged from Sprint 12. The test
  asserts command-line invariance, not byte-identical JSON.
- A round-trip test proving `VideoProfileRef` deserializes a legacy
  bare-string `"profile": "default-hevc"` compiled document to `Named` and
  plans successfully (compiled-policy compatibility).
- FFmpeg worker conformance tests per encoder (`libx265`, `libsvtav1`,
  `libaom-av1`): success, MKV and MP4 muxing (`hvc1`/`av01`), max-dimension
  downscale, `copy_video` copy path, the worker failing loudly when `copy_video`
  is true but its input probe shows a non-conforming source, pixel-format
  conversion, output validation mismatches (codec, container, dimensions, pixel
  format), missing input, input drift, existing output, bad payload, path
  escape, provider failure, and timeout.
- FFmpeg payload (command-shape) golden tests per encoder and per field.
- FFmpeg/ffprobe preflight tests for missing binaries, non-executable binaries,
  and a missing required encoder per encoder family.
- Control-plane dispatch tests proving the resolved profile is consumed instead
  of `default_hevc()`, that the control plane computes `copy_video` from the
  snapshot, and that a worker result whose `copied_video` disagrees with the
  requested `copy_video` is rejected.
- Control-plane unit tests for source selection, profile-aware precondition
  re-evaluation, staging path selection, retry-safe staging path uniqueness,
  target-path naming including the profile-identity discriminator (two profiles
  of the same codec+container produce distinct, coexisting targets; the same
  profile twice collides with `CONFIG_INVALID`), path canonicalization/symlink
  rejection, artifact recording, verification integration, commit integration,
  committed-result media snapshot recording, and event payload correlation.
- CLI insta snapshots for `voom profile list`, `voom profile show`, transcode
  execution reports with profile facts, and an unknown-profile error.
- Integration tests for scan -> policy plan -> execute -> transcode -> verify ->
  commit -> result snapshot using small fixture media, covering named and inline
  profiles, HEVC and AV1, and MKV and MP4 outputs.
- Documentation placeholder scan.
- `just ci`.

The CI FFmpeg build must provide `libx265`, `libsvtav1`, and `libaom-av1`;
absence is a setup failure, not a skipped test.

Tests must follow the repository layout convention: sibling `*_test.rs` files
for unit tests and integration tests under `crates/*/tests/`.

## 11. Acceptance Criteria

Sprint 15 is complete when:

- A policy referencing a named built-in profile, and a policy specifying inline
  settings, both compile and plan for HEVC and AV1 targets.
- `transcode video to hevc` with no profile still resolves to `default-hevc` and
  produces the same FFmpeg command line as Sprint 12.
- An existing compiled policy with a legacy bare-string `profile` deserializes
  and plans unchanged.
- Invalid inline settings, unknown named profiles, and MP4 targets with
  MP4-incompatible non-video streams all block visibly with stable diagnostics.
- The planner is dimension-, pixel-format-, and container-aware: an
  already-target-codec source that exceeds a dimension cap, mismatches a
  constrained pixel format, or needs a container change is planned, not a no-op.
- The planner emits resource/quality estimate notes derived from the resolved
  profile.
- The control plane resolves named profiles before planning and carries the
  resolved profile through dispatch to the worker.
- The FFmpeg worker applies all fields per encoder, muxes MKV and MP4,
  downscales aspect-preserving and downscale-only, and stream-copies the video
  when `copy_compatible` fires, reporting `copied_video` and observed output
  dimensions/pixel format.
- Output codec/container/dimensions/pixel-format mismatches and source drift are
  visible and do not report success.
- Missing required encoders fail during worker preflight with explicit
  diagnostics; required CI tests are not skipped.
- The worker writes only a staged output and never commits managed media state;
  the control plane records the staged artifact, verifies it, commits it
  add-only, and records the resulting `FileVersion`, `FileLocation`, and
  committed-result `MediaSnapshot`.
- `voom profile list` and `voom profile show <name>` return stable JSON
  envelopes and unknown names return `NOT_FOUND`.
- CLI golden tests lock the agent-facing envelope shape.
- The Sprint 15 closeout matrix records repeatable evidence for the profile
  model, validation, resolution, planning, payload generation, worker
  application, and operator inspection.
- `just ci` passes.

## 12. Deferred Work

Deferred to later pre-daemon sprints:

- The `QualityScoringProfile` registry, `QualityScore` records, and quality
  scoring computation.
- User-defined video profile create/update.
- mov_text subtitle transcoding and attachment handling for MP4 output.
- Hardware-accelerated encoders, tuning databases, additional software
  encoders/codecs, fractional CRF, bitrate/codec ladders, and per-title
  automatic profile selection.
- Sprint 16 multi-phase real-media policy workflow completion.
- Sprint 17 backup, sidecar ingest, and real-media CLI closeout.
