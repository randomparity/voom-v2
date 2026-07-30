# Apple VideoToolbox video acceleration

Issue: #411

## Goal

Allow an operator on a supported Apple-silicon Mac to choose typed H.264 or
HEVC VideoToolbox profiles and dispatch them only to a local FFmpeg worker
bound to that host's tested VideoToolbox resource. Support explicit software
decode and VideoToolbox decode without changing existing software or NVIDIA
profile behavior.

## Success criteria

- Existing software and NVIDIA profile JSON, validation, commands, scheduling,
  assignments, and durable evidence remain behaviorally unchanged.
- `h264_videotoolbox` and `hevc_videotoolbox` profiles require a positive
  bitrate and accept only a closed, encoder-specific vocabulary.
- VideoToolbox decode is explicit and never selected as a fallback.
- Run-local records the hashed platform identity and advertises only
  encoder/decoder/format combinations proven by real FFmpeg commands.
- The worker refuses software encoding with `-allow_sw 0`.
- H.264, HEVC, and AV1 VideoToolbox decode retain
  `videotoolbox_vld` frames through optional `scale_vt` and the VideoToolbox
  encoder. A bit-depth mismatch fails before execution rather than inserting
  an implicit system-memory conversion.
- Policy preflight and per-file scheduling require a live compatible
  VideoToolbox descriptor and tested spare capacity.
- Capacity is aggregated by stable host resource token and rechecked
  atomically during lease acquisition.
- macOS claim recovery never signals a process whose identity is ambiguous.
- Requests, results, events, reports, and hardware acceptance retain the exact
  assignment and verified output facts.
- Unit, migration, conformance, and real-media tests cover malformed profiles,
  command shapes, unavailable capabilities, capacity, recovery, evidence, and
  no-fallback behavior.
- `just ci` and the VideoToolbox acceptance command complete without warnings.

## Approved contract

The operator approved these decisions before implementation:

- H.264 and HEVC VideoToolbox encode profiles use `bitrate_kbps`.
- H.264, HEVC, and AV1 are the VideoToolbox decode codecs in scope.
- `voom worker run-local` gains `--videotoolbox` and an optional declared
  capacity.
- The stable resource identifier is a SHA-256 digest of `IOPlatformUUID`; the
  raw platform UUID is never persisted.
- Existing accelerator descriptors and claims become backend-neutral through
  a migration, not a compatibility shim.
- Same-boot macOS recovery is conservative: it transfers a claim only after
  both the recorded PID and process group are absent.
- This slice supports Apple silicon only. An Intel Mac fails preflight with an
  actionable unsupported-platform error until matching hardware acceptance
  exists.

## Dependencies and exclusions

This design extends the accelerator resource, capacity, assignment, and
recovery model accepted in ADR 0049. It depends on:

- exact worker-protocol version matching from ADR 0016;
- additive durable payload evolution from ADR 0013;
- store-owned atomic lease capacity checks;
- existing source-codec facts, output probing, artifact verification, and
  commit behavior.

Excluded:

- ProRes output profiles;
- Intel Mac and discrete/multi-GPU selection;
- automatic discovery of a theoretical VideoToolbox session maximum;
- VideoToolbox decode feeding a software encoder;
- automatic retry with software encode or decode;
- remote-worker VideoToolbox configuration.

These exclusions do not weaken this slice: none is required to execute the two
accepted encoders and three accepted decoders on the tested Apple-silicon
resource. A future platform issue must supply its own matching hardware
evidence before extending the vocabulary.

## Profile contract

`TranscodeVideoProfile` and policy inline settings gain:

```text
bitrate_kbps: Option<u32>
```

Quality fields are mutually exclusive:

- software encoders require `crf`;
- `hevc_nvenc` requires `cq`;
- VideoToolbox encoders require positive `bitrate_kbps`;
- every encoder rejects the other two quality fields.

The descriptor quality domain gains `BitrateKbps`. It has a minimum of one and
uses the full `u32` range; VOOM does not invent a lower platform maximum.
SQLite stores the value as an integer and checks `1..=4294967295`.

VideoToolbox encoder vocabulary:

| Encoder | Target | Preset | Profiles | Levels | Pixel formats |
|---|---|---|---|---|---|
| `h264_videotoolbox` | `h264` | `default` | `high` | `4.1` | `yuv420p` |
| `hevc_videotoolbox` | `hevc` | `default` | `main`, `main10` | none | `yuv420p`, `yuv420p10le` |

Every tuple field in this table is required for a VideoToolbox profile.
H.264 requires `codec_profile=high`, `codec_level=4.1`, and
`pixel_format=yuv420p`. HEVC rejects `codec_level` and accepts only the paired
combinations `main`/`yuv420p` or `main10`/`yuv420p10le`. Omitted or crossed
profile/pixel-format values fail descriptor validation. This makes output bit
depth available to planning and command construction without relying on
FFmpeg defaults.

`preset=default` emits no encoder option. VideoToolbox's `prio_speed`,
`power_efficient`, and `spatial_aq` controls are excluded: unsupported devices
can accept the option and emit a warning while ignoring it, which is
incompatible with a deterministic zero-warning contract.

H.264 becomes a supported transcode target. Matroska needs no new muxer option;
MP4 uses the existing explicit tag path with `avc1`. Existing HEVC and AV1
container commands are unchanged.

The tagged decode enum gains:

```json
{"backend":"video_toolbox"}
```

The serde variant is a newtype over a `deny_unknown_fields` content struct.
Software remains the omitted default. A VideoToolbox decode mode requires a
VideoToolbox encoder in this slice.

No VideoToolbox profile is seeded. Operators choose bitrate, output bit depth,
decode mode, and declared host capacity explicitly.

## Source format contract

Hardware decode compatibility is a codec-and-format pair, not an encoder-list
claim. The descriptor records each successfully executed source combination:

```text
VideoToolboxDecodeCapability {
    codec,
    pixel_formats,
}
```

Canonical source formats are:

- eight-bit 4:2:0: `yuv420p` or `nv12`;
- ten-bit 4:2:0: `yuv420p10le` or `p010le`.

The planner threads both `source_video_codec` and
`source_video_pixel_format` into each transcode ticket. The worker request
echoes those expected facts, and the worker's own input probe must match before
FFmpeg starts.

VideoToolbox decode is accepted only when input and output bit depth agree:

- H.264 output is eight-bit only;
- HEVC Main output requires an eight-bit source;
- HEVC Main 10 output requires a ten-bit source.

This constraint avoids a hidden download, software format conversion, and
re-upload. A mismatched file is a per-file planning block when facts are
available and a fail-loud request error if planning was bypassed.

## Backend-neutral accelerator contract

The concrete NVIDIA descriptor becomes:

```text
#[serde(tag = "backend", rename_all = "snake_case")]
VideoAcceleratorDescriptor {
    Nvidia(NvidiaVideoAcceleratorDescriptor),
    VideoToolbox(VideoToolboxVideoAcceleratorDescriptor),
}
```

Both content structs deny unknown fields. Migration 0031 adds
`backend: "nvidia"` to every stored accelerator capability before the new
tagged type reads it. There is one current format after migration.

The VideoToolbox content contains:

- `hardware_token`: `videotoolbox:<resource_id>`;
- `resource_id`: lowercase SHA-256 of the normalized `IOPlatformUUID`;
- `model_identifier`;
- `chip_name`;
- `macos_version` and build;
- proven encoder names;
- proven codec/format decoder capabilities;
- tested `max_sessions`.

The digest is an identity token, not a credential. The raw platform UUID,
serial number, hardware UUID, and user name are not persisted or logged.

`VideoHardwareRequirement` and `VideoHardwareAssignment` gain newtype
VideoToolbox variants. A VideoToolbox assignment contains the exact hardware
token and resource ID. NVIDIA retains its UUID fields and serialized
assignment shape.

The durable transcode-success payload adds optional
`hardware_resource_id`. NVIDIA continues to populate its existing
`hardware_device_uuid`; VideoToolbox populates the generic resource ID. The
complete typed assignment remains present in the worker result and execution
report. The new durable field is added to the payload contract inventory and
guard scope required by ADR 0013.

## Worker configuration

Existing commands remain:

```text
voom worker run-local --kind ffmpeg

voom worker run-local --kind ffmpeg \
  --nvidia-device <GPU-uuid> \
  --nvidia-max-sessions <1..=16>
```

VideoToolbox adds:

```text
voom worker run-local --kind ffmpeg \
  --videotoolbox \
  --videotoolbox-max-sessions <1..=16, default 1>
```

`--videotoolbox` conflicts with `--nvidia-device`.
`--videotoolbox-max-sessions` requires `--videotoolbox`. Accelerator options
remain valid only for an FFmpeg worker.

The supervisor reads `IOPlatformUUID`, normalizes it as uppercase canonical
UUID text, hashes it, forms the stable token, and acquires the durable claim
before spawning the worker. It passes only the expected digest and declared
capacity to the child. The worker independently reads and hashes the platform
identity and refuses readiness when the digest differs.

Production probes use absolute macOS system-tool paths. Test-only overrides
remain explicit environment boundaries so fixtures cannot accidentally execute
host tools.

## Claim schema and macOS recovery

Migration 0031 rebuilds `accelerator_claims` with:

- backend in `nvidia | video_toolbox`;
- `supervisor_start_identity TEXT NULL` instead of the Linux-specific integer;
- all existing ownership, process-group, capacity, and time fields.

Existing NVIDIA rows copy their start value as
`linux-proc-ticks:<ticks>`. NVIDIA behavior remains unchanged.
VideoToolbox rows use `NULL` because the workspace forbids unsafe Rust and the
safe standard library exposes no precise macOS process-start identity.

The macOS claim records the boot-session UUID, worker PID, process-group ID,
and capacity. Recovery is deliberately conservative:

1. A different boot-session UUID proves every prior process is gone. Retire the
   old owner without signalling numeric IDs.
2. On the same boot, if any process currently owns the recorded PID, refuse
   recovery. PID reuse produces a safe false conflict, never a stolen claim.
3. If the PID is absent but any member remains in the recorded process group,
   refuse recovery and report the group for manual cleanup. The implementation
   never signals that ambiguous group.
4. Only when both PID and group are absent may it retire the old worker and
   transfer the claim.

An inability to inspect either PID or process-group state preserves the old
claim and fails recovery. Inspection failure is not evidence of absence.

The normal shutdown path still owns its child handle and retires the worker
after the child exits. Startup failure kills and reaps the known child; a claim
is retained if group cleanup cannot be proved.

This differs intentionally from Linux NVIDIA recovery, which has a precise
`/proc` start identity and may terminate a proven orphan group. The two
backends share claim uniqueness, not unsupported OS assumptions.

## Worker preflight

The FFmpeg worker first runs the existing software dependency and muxer checks.
For a VideoToolbox configuration it then:

1. requires macOS on Apple silicon and records model, chip, OS version/build,
   and the expected hashed resource ID;
2. requires the `videotoolbox` hardware accelerator;
3. requires the `h264_videotoolbox` and `hevc_videotoolbox` encoder entries and
   the `scale_vt` filter;
4. creates private H.264 eight-bit, HEVC eight/ten-bit, and AV1
   eight/ten-bit fixtures;
5. decodes each fixture using
   `-hwaccel videotoolbox -hwaccel_output_format videotoolbox_vld` and encodes
   it with `hevc_videotoolbox -allow_sw 0`;
6. retains only codec/format pairs whose real pipeline succeeds;
7. proves the declared capacity concurrently for three software-decode encode
   paths: H.264 High, HEVC Main, and HEVC Main 10;
8. proves the same declaration concurrently for every retained
   hardware-decoder codec/format path, targeting the matching H.264 High, HEVC
   Main, or HEVC Main 10 encoder;
9. advertises the declaration only if every homogeneous group succeeds.

Individual decoder-format failures are diagnostic omissions. Startup requires
both accepted output encoders and at least one tested decode capability.
Failure of any concurrent group fails the declaration rather than publishing a
capacity inferred from a different codec or bit depth. The operator can retry
with a smaller declaration.

Every external process has a 15-second deadline and is killed and reaped on
timeout. Fixture and log files live in a mode-0700 temporary directory removed
on every success or failure path. Each declared group runs concurrently and
counts as one preflight stage.

Capacity inputs are three seconds long and use FFmpeg's realtime input pacing
plus machine-readable progress output. After spawning the complete group, the
probe waits until every child has reported at least one encoded frame, then
requires every process still to be alive at that same observation point. An
early non-zero exit fails the declaration. An early successful exit is
inconclusive and also fails, because it did not prove overlap. Only
first-frame evidence from every child, an all-live observation, and subsequent
successful deadline-bounded exits prove the group.

The complete plan has at most 25 sequential stages: four existing FFmpeg/
FFprobe inventory stages, platform identity, hardware-accelerator inventory,
filter inventory, five fixtures, five single decoder-format probes, three
software-decode capacity groups, and five hardware-decode capacity groups.
With 15 seconds per stage plus 30 seconds of coordination, the supervisor
deadline is 405 seconds:

```text
25 * 15 seconds + 30 seconds = 405 seconds
```

The stage count, deadline, and invariant live in code and remain below the
existing 15-minute accelerator-recovery window.

No encoder or decoder is advertised from `ffmpeg -encoders`, `-hwaccels`, or
`-filters` text alone. Those inventories are prerequisites; executed media is
the authority.

## Scheduling and capacity

Resolved profiles derive one requirement:

- software encoder: no accelerator;
- NVENC: existing NVIDIA encoder/optional decoder requirement;
- VideoToolbox plus software decode: exact VideoToolbox encoder;
- VideoToolbox plus VideoToolbox decode: exact encoder and tested source
  codec/format pair.

Policy preflight requires at least one live endpoint for every distinct
profile-level backend and encoder. A VideoToolbox decode profile additionally
requires at least one advertised decoder capability; exact source matching
remains per file.

Candidate projection parses the tagged descriptor. Software candidates must
remain unaccelerated. VideoToolbox candidates require:

- the exact backend and encoder;
- the stable token in `worker_capabilities.hardware`;
- a matching decoder capability when requested;
- no conflicting live capacity declarations for the token.

The selected assignment is inserted only after compatibility succeeds.
Equal eligible loads retain worker-ID tie breaking.

Capacity remains token-wide. Candidate reads and atomic lease acquisition count
every held `transcode_video` lease whose worker advertises the same hardware
token. Duplicate worker rows cannot multiply capacity. Saturation defers
without consuming an attempt.

Existing token-keyed accelerator recovery applies. A historically matching but
temporarily absent VideoToolbox worker defers without ticket mutation; a
replacement with the same host token resumes the run. A never-advertised
requirement retains the ordinary no-eligible-worker backstop.

## Deterministic FFmpeg commands

All VideoToolbox encoder commands include:

```text
-c:v <h264_videotoolbox|hevc_videotoolbox>
-allow_sw 0
-b:v <bitrate_kbps>k
```

Profile and level options are emitted only when accepted by the selected
descriptor. The worker emits exactly one video filter graph.

Software decode:

```text
-i <input>
...maps...
-vf [scale=... ,]format=<nv12|p010le>
-c:v <videotoolbox encoder> -allow_sw 0 -b:v <n>k ...
```

The scale expression is downscale-only, preserves aspect ratio, and produces
even dimensions. The final explicit format creates the pixel-buffer layout
accepted by VideoToolbox. No `hwupload` is required because the encoder accepts
the software pixel buffer.

VideoToolbox decode without scaling:

```text
-hwaccel videotoolbox
-hwaccel_output_format videotoolbox_vld
-i <input>
...maps...
-c:v <videotoolbox encoder> -allow_sw 0 -b:v <n>k ...
```

VideoToolbox decode with scaling:

```text
-hwaccel videotoolbox
-hwaccel_output_format videotoolbox_vld
-i <input>
...maps...
-vf scale_vt=w=<even downscale width>:h=<even downscale height>
-c:v <videotoolbox encoder> -allow_sw 0 -b:v <n>k ...
```

The hardware path contains no `hwdownload`, `hwupload`, `format`, `scale`, or
auto-inserted system-memory filter. Verbose acceptance requires FFmpeg to
report `pixfmt:videotoolbox_vld` and
`Using input frames context (format videotoolbox_vld)`.

Any process failure is returned through the existing typed worker failure. No
branch changes the profile, retries another encoder, drops hardware-decode
arguments, or sets `-allow_sw 1`.

## Migration and rollback

Migration 0030 rebuilds `video_profiles`:

- the table gains nullable `bitrate_kbps`, H.264 target/encoder vocabulary, and
  `video_toolbox` decode;
- one explicit-column copy preserves every existing ID, name, field, and
  `retired_at`;
- old rows receive `bitrate_kbps = NULL`.

Migration 0031 rebuilds `accelerator_claims` with the backend-neutral
process-start field and VideoToolbox backend. Existing NVIDIA rows preserve
every owner/capacity fact and receive the prefixed Linux start identity. It
also adds the required `backend: "nvidia"` tag to stored accelerator capability
JSON. The update is limited to rows with an accelerator object and no backend
tag. Tests cover active and retired capability history.

Neither migration reruns seed inserts. Upgrade remains binary before
database: the schema-version guard prevents a new binary from writing an old
schema and an old binary from opening a newer schema. Rollback is database
restore plus the old binary; there is no down migration.

## Failure behavior

- Non-macOS or Intel Mac: startup fails with the supported Apple-silicon
  requirement and recorded observed platform.
- Missing platform identity, FFmpeg feature, hardware permission, encoder,
  decoder, or format: startup names the failed probe and advertises no partial
  capability.
- Encoder hardware unavailable or busy: `-allow_sw 0` makes the smoke or
  operation fail; software encode is never accepted.
- Declared concurrency unavailable: startup names the declaration and failed
  child; no worker capability is recorded.
- A capacity child exits before the all-live overlap observation: startup
  rejects the declaration, including when the early exit was successful and
  therefore inconclusive.
- Live or ambiguous same-boot claim: startup reports the PID/group and refuses
  claim transfer.
- PID or process-group inspection failure: startup preserves the claim and
  reports the failed inspection; it never treats an error as absence.
- Different boot or absent PID and group: old worker retires before the new
  claim is inserted.
- Missing compatible worker at run start: policy preflight fails before
  opening a job.
- Unsupported source codec/format or bit-depth conversion: per-file planning
  block, or fail-loud request validation after a preflight bypass.
- Temporarily absent resource: existing token-keyed recovery deferral and
  timeout behavior.
- Conflicting capacity declarations: preflight blocks a new run; a mid-run
  conflict rejects only the affected ticket.
- Assignment mismatch or missing result evidence: malformed worker result.
- Output codec, dimensions, profile, or pixel format mismatch: existing output
  fact rejection; artifact commit does not run.

## Security and privacy

- The platform UUID is read locally, hashed, and discarded. It is never placed
  in capability JSON, events, logs, CLI output, or acceptance records.
- The resource digest proves identity only; worker authentication continues to
  use the existing secret and exact worker identity protocol.
- Profile fields map through closed enums and numeric types. No arbitrary
  FFmpeg argument reaches a process.
- Production platform-tool paths are absolute. Test overrides are explicit and
  never serialized.
- Probe directories are private and cleaned on every path.
- Strict serde content structs reject unknown request, descriptor, assignment,
  and durable payload fields.

## Observability and acceptance evidence

Startup errors name the platform or media probe that failed. The capability
records model identifier, chip, OS version/build, proven codecs/formats, and
tested capacity. Assignment evidence records the stable digest but not the raw
platform UUID.

The repository acceptance script records:

- Mac model identifier and Apple chip;
- macOS version/build;
- FFmpeg version and `--enable-videotoolbox`;
- worker readiness descriptor;
- software-decode H.264 and HEVC encodes;
- VideoToolbox-decode H.264, HEVC, and AV1 encodes;
- an HEVC Main 10 path;
- a scaled `scale_vt` path;
- declared concurrent pipelines;
- FFprobe output codec/profile/dimensions/pixel format;
- verbose proof of a `videotoolbox_vld` frames context;
- absence of `hwdownload`, `hwupload`, auto-scale, and software encoder
  fallback.

## Acceptance-host evidence

The pre-design commands ran on:

- MacBook Pro `Mac17,6`;
- Apple M5 Max with 40-core GPU;
- macOS 26.5.2 build 25F84;
- FFmpeg 8.1.2 built with `--enable-videotoolbox`.

Observed:

- H.264 and HEVC encode succeeded with `-allow_sw 0`;
- H.264, HEVC Main 10, and AV1 eight/ten-bit decode produced
  `videotoolbox_vld` frames consumed directly by
  `hevc_videotoolbox`;
- H.264 hardware decode fed `h264_videotoolbox` directly and produced
  H.264 High level 4.1;
- software H.264 decode plus explicit `format=p010le` produced HEVC Main 10;
- `scale_vt` downscaled 640x360 to 320x180 while retaining the hardware frames
  context;
- output facts matched H.264 High, HEVC Main, and HEVC Main 10 expectations;
- 16 simultaneous H.264-decode to HEVC-encode pipelines succeeded.

The accepted declaration ceiling is therefore 16 for this host. This is not a
portable theoretical maximum.

FFmpeg 8.1.2 source makes `allow_sw = 0` the default and sets
`RequireHardwareAcceleratedVideoEncoder = true`; the command still emits
`-allow_sw 0` so the product contract is explicit.

## Test strategy

- Descriptor tests: accepted/rejected bitrate, encoder, preset, profile, level,
  target, pixel format, and decode combinations.
- Serde tests: strict tagged decode, descriptor, requirement, and assignment
  variants; existing software/NVIDIA snapshots remain stable except for the
  migrated internal capability tag.
- Migration tests: exact row preservation, H.264/VideoToolbox constraints,
  NVIDIA claim conversion, capability JSON tagging, and malformed insertion
  rejection.
- Platform/preflight tests: mocked tool outputs, raw UUID non-disclosure,
  encoder/decoder/format omission, timeouts, cleanup, concurrency failures,
  unsupported OS/architecture, and actionable messages.
- Recovery tests: boot change, live/reused PID, absent PID with live group,
  fully absent owner, claim retention on ambiguous cleanup, and unchanged Linux
  NVIDIA recovery.
- Scheduler tests: software/NVIDIA/VideoToolbox isolation, exact
  codec/format matching, deterministic ties, token-wide capacity, conflicts,
  historical recovery, and no-compatible-worker behavior.
- Command golden tests: both encoders, both decode modes, bit depths, scaling,
  MP4 tags, malformed requests, and explicit no-fallback arguments.
- Evidence tests: assignment equality, result/event/report resource identity,
  output mismatches, and unchanged NVIDIA evidence.
- Real acceptance: execute the repository script on the recorded Mac, then run
  `just ci`.
