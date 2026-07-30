# Apple VideoToolbox video acceleration implementation plan

Issue: #411

Base branch: `main`

Full guardrail: `just ci`

Design:
`docs/superpowers/specs/2026-07-29-issue-411-videotoolbox-video-acceleration-design.md`

ADR: `docs/adr/0051-videotoolbox-is-a-host-scoped-accelerator-resource.md`

## Prerequisite — Acceptance-host command evidence

Completed before implementation. The design records executed FFmpeg 8.1.2
commands on an Apple M5 Max running macOS 26.5.2 for:

- H.264 and HEVC hardware encode with `-allow_sw 0`;
- H.264, HEVC Main 10, and AV1 eight/ten-bit hardware decode;
- direct VideoToolbox decode-to-encode without a system-memory transition;
- `scale_vt` downscaling while retaining VideoToolbox frames;
- explicit software-frame conversion for HEVC Main 10 encode; and
- 16 concurrent H.264-decode-to-HEVC-encode pipelines.

The implementation must reproduce these command shapes and output facts. The
observed capacity is evidence for this host, not a portable default.

## Step 0 — Record the approved architecture

Files:

- `docs/adr/0051-videotoolbox-is-a-host-scoped-accelerator-resource.md`
- `docs/adr/README.md`
- the design and this implementation plan under `docs/superpowers/specs/`

Behavior:

- Record the approved host-scoped identity, backend-neutral schema, strict
  profile vocabulary, conservative macOS recovery, executed-capability probes,
  deterministic command transitions, and acceptance evidence.
- Keep ADR 0049's token-wide scheduling model and ADR 0016's exact protocol
  version matching as dependencies rather than restating them.

Verification:

```text
./scripts/check-adr-index.sh
git diff --check
```

Commit: `docs(video): design VideoToolbox acceleration`

## Step 1 — Typed profile and wire vocabulary

Files:

- `crates/voom-core/src/media/encoder_caps.rs` and sibling tests
- `crates/voom-core/src/media/transcode_video_profile.rs` and sibling tests
- `crates/voom-policy/src/data/video_profile.rs` and sibling tests
- `crates/voom-policy/src/compile/lower/operations.rs` and sibling tests
- `crates/voom-worker-protocol/src/video_acceleration.rs` and sibling tests
- `crates/voom-worker-protocol/src/operations/transcode_video.rs` and sibling
  tests
- `crates/voom-store/src/repo/policy/video_profiles.rs` and sibling tests
- `crates/voom-cli/src/cli.rs`
- `crates/voom-cli/src/commands/media/profile.rs` and sibling tests
- `migrations/0030_videotoolbox_video_profiles.sql`
- `crates/voom-store/src/migrator.rs`
- `crates/voom-store/tests/migration_inventory.rs`

Behavior:

- Add `BitrateKbps`, H.264 target/container support, the two closed
  VideoToolbox encoder descriptors, and strict profile/level/pixel-format
  tuples.
- Add the strict `video_toolbox` decode variant and source codec/pixel-format
  request facts without changing omitted software-decode serialization.
- Add tagged VideoToolbox requirement, assignment, and descriptor content
  types. Keep the existing NVIDIA serialized requirement and assignment shapes.
- Add CLI and policy `bitrate_kbps` input with mutual exclusion across CRF, CQ,
  and bitrate.
- Rebuild `video_profiles` with an explicit-column migration that preserves
  every existing row and retirement marker and does not rerun seeds.

Red tests and expected failure:

- Core descriptor tests for accepted H.264 High 4.1 and paired HEVC Main/Main
  10 tuples fail because the encoders, bitrate domain, and H.264 target do not
  exist yet.
- Malformed/crossed tuple, missing bitrate, conflicting quality, and strict
  decode serde tests fail because current validation knows only software and
  NVIDIA.
- Protocol round-trip tests for VideoToolbox requirements, assignments,
  descriptors, and source pixel format fail because the wire types are absent.
- Migration preservation and invalid-insert tests fail because migration 0030
  and its constraints are absent.
- CLI/policy tests fail because `--bitrate-kbps` and `video_toolbox` are not
  accepted.

Verification:

```text
cargo test -p voom-core
cargo test -p voom-policy video_profile
cargo test -p voom-worker-protocol video
cargo test -p voom-store video_profile
cargo test -p voom-store migration_inventory
cargo test -p voom-cli profile
```

Cleanup:

- Replace encoder-specific quality branching with exhaustive typed matching
  where all three quality domains now apply; do not retain parallel legacy
  validation.
- Update all compile-required profile literals in the same commit without
  weakening their assertions.

Commit: `feat(video): add VideoToolbox profile contracts`

## Step 2 — Backend-neutral descriptors and durable claims

Files:

- `crates/voom-worker-protocol/src/video_acceleration.rs` and sibling tests
- `crates/voom-control-plane/src/video_hardware.rs` and new sibling tests
- `crates/voom-store/src/repo/execution/accelerator_claims.rs` and sibling
  tests
- `crates/voom-store/src/repo/execution/workers.rs` and sibling tests
- `migrations/0031_backend_neutral_accelerator_claims.sql`
- `crates/voom-store/src/migrator.rs`
- `crates/voom-store/tests/migration_inventory.rs`

Behavior:

- Replace the concrete startup accelerator field with the strict tagged
  `VideoAcceleratorDescriptor`; add `backend: "nvidia"` to migrated stored
  NVIDIA capability JSON.
- Rebuild `accelerator_claims` with backend-neutral start identity and the
  `nvidia | video_toolbox` backend constraint.
- Convert existing NVIDIA start ticks to `linux-proc-ticks:<ticks>` while
  retaining every claim owner, token, process group, capacity, and timestamp.
- Parse candidate capabilities through the tagged type. Reject malformed or
  unknown accelerator descriptors with backend-neutral context.

Red tests and expected failure:

- Active and retired worker-capability migration tests fail because stored
  NVIDIA JSON is untagged.
- Claim migration tests fail because the table accepts only NVIDIA and exposes
  a Linux-specific integer start field.
- Strict tagged-descriptor and malformed-candidate tests fail because the
  current parser assumes every accelerator is NVIDIA.
- Existing NVIDIA round trips guard against an accidental wire-shape change.

Verification:

```text
cargo test -p voom-worker-protocol video_acceleration
cargo test -p voom-store accelerator_claim
cargo test -p voom-store migration_inventory
cargo test -p voom-control-plane video_hardware
```

Cleanup:

- Remove direct `NvidiaVideoAcceleratorDescriptor` parsing at generic startup,
  registration, and candidate boundaries.
- Do not keep an untagged-descriptor fallback after migration 0031.

Commit: `refactor(video): generalize accelerator resources`

## Step 3 — macOS identity, preflight, and claim recovery

Files:

- `crates/voom-cli/src/cli.rs`
- `crates/voom-cli/src/commands/execution/worker.rs` and sibling tests
- `crates/voom-control-plane/src/local_worker.rs` and sibling tests
- `crates/voom-control-plane/src/worker_process.rs` and sibling tests where
  process inspection is shared
- `crates/voom-ffmpeg-worker/src/main.rs` and sibling tests
- `crates/voom-ffmpeg-worker/src/preflight.rs` and sibling tests

Behavior:

- Add mutually exclusive `--videotoolbox` and NVIDIA worker configuration,
  with a declared VideoToolbox capacity of `1..=16` defaulting to one.
- Read the platform UUID with an absolute macOS tool, normalize it, hash it
  with the existing workspace `sha2` dependency, and discard the raw value.
- Pass only the expected resource digest and capacity to the child; require the
  child to independently verify the identity.
- Require Apple silicon, the accepted FFmpeg inventories, both encoders,
  `scale_vt`, and real codec/format pipelines.
- Prove every advertised path at the declared capacity with realtime progress,
  first-frame evidence, an all-live observation, deadline-bounded success, and
  private temporary-fixture cleanup.
- Derive the 465-second readiness deadline from 29 maximum sequential
  15-second stages plus 30 seconds of coordination, and assert that the
  configured timeout covers that complete budget.
- Recover same-boot claims only when both PID and process group are confirmed
  absent. Preserve the claim on liveness-inspection errors or ambiguous groups;
  never signal an ambiguous macOS process group.

Red tests and expected failure:

- CLI exclusivity/range tests fail because VideoToolbox flags do not exist.
- Platform fixtures fail because no Apple-silicon identity reader or stable
  token builder exists.
- Raw-UUID non-disclosure tests fail until error, readiness, and descriptor
  paths carry only the digest.
- Inventory-only, missing encoder/filter, unusable codec/format, early-success,
  early-failure, non-overlap, timeout, and cleanup tests fail because current
  preflight has only NVIDIA probes.
- A stage-budget assertion fails until the 29-stage maximum, per-stage
  deadline, coordination allowance, and worker readiness timeout are connected
  in code.
- Boot-change, live/reused PID, live orphan group, absent owner, and inspection
  failure tests fail because recovery currently assumes Linux `/proc` start
  ticks.
- Existing Linux NVIDIA recovery tests remain green.

Verification:

```text
cargo test -p voom-cli worker
cargo test -p voom-control-plane local_worker
cargo test -p voom-control-plane worker_process
cargo test -p voom-ffmpeg-worker preflight
cargo test -p voom-ffmpeg-worker main
```

Cleanup:

- Isolate OS-specific process identity and recovery behind explicit enum
  branches; do not make Linux sentinel values represent macOS semantics.
- Keep test-only tool overrides out of serialized production configuration.
- Split probe orchestration into functions under the repository's line and
  complexity limits only where distinct tested behavior warrants it.

Commit: `feat(worker): probe VideoToolbox capabilities`

## Step 4 — Compatibility, planning, and token-wide capacity

Files:

- `crates/voom-plan/src/planner/transcode_video/mod.rs`
- `crates/voom-plan/src/planner/transcode_video/profile.rs` and sibling tests
- `crates/voom-control-plane/src/workflow/plan/binding.rs` and sibling tests
- `crates/voom-control-plane/src/cases/policy/tool_preflight.rs` and sibling
  tests
- `crates/voom-control-plane/src/workflow/execution/executor/spawn.rs` and
  sibling tests
- `crates/voom-store/src/repo/execution/workers.rs` and sibling tests
- `crates/voom-store/src/repo/execution/leases.rs` and sibling tests

Behavior:

- Thread observed source codec and pixel format into each transcode ticket and
  derive an exact VideoToolbox requirement.
- Block mismatched source/output bit depth during planning when facts are
  present.
- Require live profile-level encoder/decode capability during policy preflight,
  then enforce exact codec/format compatibility per file.
- Select only exact backend/token/resource matches and retain worker-ID
  tie-breaking.
- Aggregate all `transcode_video` leases by stable hardware token and recheck
  descriptor compatibility plus capacity atomically at acquisition.
- Reuse ADR 0049 token-keyed unavailability recovery for a historically
  matching VideoToolbox host.

Red tests and expected failure:

- Software/NVIDIA/VideoToolbox isolation tests fail because candidate matching
  understands only software and NVIDIA.
- H.264/HEVC/AV1 format pairs and bit-depth mismatch tests fail because tickets
  do not carry source pixel format.
- Duplicate-worker token saturation and concurrent-acquisition tests fail if
  capacity is incorrectly counted per worker row.
- Conflicting declarations, deterministic tie, never-advertised, temporarily
  absent, and replacement-host tests fail until the generic descriptor is used
  throughout selection and recovery.

Verification:

```text
cargo test -p voom-plan transcode_video
cargo test -p voom-control-plane binding
cargo test -p voom-control-plane tool_preflight
cargo test -p voom-control-plane executor
cargo test -p voom-store worker
cargo test -p voom-store lease
```

Cleanup:

- Replace NVIDIA-only requirement matches at generic scheduler boundaries with
  exhaustive backend matching.
- Keep codec/format rules in typed compatibility functions rather than
  duplicating string comparisons in planner, scheduler, and worker.

Commit: `feat(scheduler): assign VideoToolbox resources`

## Step 5 — Deterministic FFmpeg commands and durable evidence

Files:

- `crates/voom-worker-protocol/src/operations/transcode_video.rs` and sibling
  tests
- `crates/voom-ffmpeg-worker/src/ffmpeg.rs` and sibling tests
- `crates/voom-ffmpeg-worker/src/handler.rs` and sibling tests
- `crates/voom-ffmpeg-worker/tests/transcode_conformance.rs`
- `crates/voom-control-plane/src/transcode/dispatch.rs` and sibling tests
- `crates/voom-control-plane/src/workflow/execution/dispatch.rs`
- `crates/voom-plan/src/compliance/model.rs` and sibling tests
- `crates/voom-plan/src/compliance/report.rs` and sibling tests
- `crates/voom-events/src/payload/artifact.rs` and sibling tests
- `docs/payload-contract-inventory.md`
- `scripts/payload-contract-scope.txt`

Behavior:

- Build the exact software-decode upload or VideoToolbox-frame-preserving
  command for H.264 and HEVC encoders.
- Always emit `-allow_sw 0`, positive `-b:v`, and the accepted profile tuple.
  Emit `avc1` for H.264 MP4.
- Use one software filter graph ending in `nv12`/`p010le`, or one hardware
  `scale_vt` graph when downscaling. Never insert hardware download/upload,
  software format/scale, or an encoder fallback into the hardware-decode path.
- Reprobe and validate expected source facts before starting FFmpeg.
- Echo and validate the exact assignment in the result.
- Add optional `hardware_resource_id` to the strict durable success payload and
  retain it in events and compliance output; preserve NVIDIA UUID evidence.

Red tests and expected failure:

- Golden argv tests for both encoders, both decode modes, both bit depths,
  scaling, no scaling, and MP4 tags fail because only software/NVENC command
  construction exists.
- Malformed request, source fact mismatch, assignment mismatch, and missing
  assignment tests fail until the worker validates the new contract.
- Negative assertions for `-allow_sw 1`, fallback encoders, `hwdownload`,
  `hwupload`, `format`, `scale`, and duplicate `-vf` fail against any implicit
  transition.
- Event/report serde tests fail because generic hardware resource evidence is
  absent.
- Existing NVIDIA command/evidence fixtures remain green.

Verification:

```text
cargo test -p voom-worker-protocol transcode_video
cargo test -p voom-ffmpeg-worker ffmpeg
cargo test -p voom-ffmpeg-worker handler
cargo test -p voom-ffmpeg-worker --test transcode_conformance
cargo test -p voom-events artifact
cargo test -p voom-plan compliance
cargo test -p voom-control-plane transcode
./scripts/check-payload-deny-unknown.sh
```

Cleanup:

- Share only the existing common command assembly; keep backend-specific frame
  transitions explicit.
- Remove NVIDIA-specific assignment assumptions from generic dispatch/evidence
  paths without changing the NVIDIA variant's public fields.

Commit: `feat(video): execute VideoToolbox transcodes`

## Step 6 — Real-host acceptance and operator documentation

Files:

- `scripts/accept-videotoolbox-video-acceleration.sh`
- `docs/runbooks/operator-real-media-execution.md`
- focused ignored real-host test support only if the CLI workflow cannot
  produce assignment and event evidence directly

Behavior:

- Verify supported platform facts and FFmpeg's VideoToolbox build feature
  without recording raw platform UUID, serial number, user name, or hardware
  UUID.
- Start a VideoToolbox run-local worker, capture its readiness descriptor, and
  execute software-decode H.264/HEVC encode plus VideoToolbox-decode
  H.264/HEVC/AV1 paths, including HEVC Main 10 and `scale_vt`.
- Verify selected assignment, durable resource evidence, FFprobe output facts,
  and verbose direct-frame evidence.
- Prove the declared concurrent capacity and reject logs containing a software
  encoder fallback, `hwdownload`, `hwupload`, or auto-scale.
- Document configuration, capacity declarations, unsupported platform,
  dependency/permission/capability failures, and conservative claim recovery.

Red tests and expected failure:

- The acceptance command initially fails because no repository workflow
  exercises the complete VideoToolbox path.
- Privacy and log-shape assertions fail if raw identity or forbidden frame
  transitions escape.
- Assignment/event assertions fail if execution drops the selected resource
  identity.

Verification:

```text
./scripts/accept-videotoolbox-video-acceleration.sh
just smoke
```

Cleanup:

- Reuse repository CLI workflows and JSON parsing conventions from the NVIDIA
  acceptance script; do not add an independent product path for acceptance.

Commit: `test(video): add VideoToolbox acceptance`

## Step 7 — Review and shipping

- Transition issue #411 to `status:in-review`.
- Review `main...HEAD` through the adversarial review loop, with explicit
  security focus on platform-identity privacy, external process arguments and
  cleanup, strict capability JSON, claim recovery, and atomic capacity races.
- Apply defensible findings and rerun the affected focused tests after each
  fix.
- Run simplification review and apply only behavior-preserving reductions.
- Run `just ci` with no skipped checks or warnings.
- Push the feature branch and create a pull request closing #411.
- Drive GitHub CI to green and a mergeable state.
- Post `WORK:REVIEW`, transition to `status:awaiting-merge`, and hand the
  mergeable pull request to the operator. Do not merge without explicit
  authorization.

Commit: review remediation only when a finding requires code changes.
