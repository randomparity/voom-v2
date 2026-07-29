# NVIDIA video acceleration implementation plan

Issue: #400

Base branch: `main`

Full guardrail: `just ci`

## Prerequisite — Acceptance-host command evidence

Completed before implementation. The design records real output from FFmpeg
8.1.2 and NVIDIA driver 595.80 on both acceptance GPUs for:

- UUID isolation and PID-to-UUID observation;
- `nvidia-smi encodersessions` during a live HEVC session;
- software decode plus CUDA upload plus HEVC NVENC; and
- H.264 CUVID decode plus `scale_cuda` plus HEVC NVENC.

Verbose logs prove the NVIDIA-decode graph remains in CUDA frames from decoder
through encoder. Step 4's golden argv fixtures come from these executed commands,
not an unexecuted proposed shape.

## Step 1 — Profile and durable registry contract

Files:

- `crates/voom-core/src/media/{encoder_caps,transcode_video_profile}.rs`
- sibling tests in `crates/voom-core/src/media/`
- `crates/voom-policy/src/data/video_profile.rs`
- policy parser/lowering validation and sibling tests
- `migrations/0029_nvidia_video_acceleration.sql`
- `crates/voom-store/src/migrator.rs`
- `crates/voom-store/src/repo/execution/accelerator_claims.rs`
- `crates/voom-store/src/repo/policy/video_profiles.rs`
- `crates/voom-cli/src/{cli.rs,commands/media/profile.rs}` and tests
- payload inventory/scope documentation

Behavior:

- Add typed CRF/CQ quality domains, an HEVC NVENC descriptor, and typed decode
  mode while preserving software serialization.
- Rebuild the profile table with nullable CRF, nullable CQ, decode backend, and
  expanded encoder constraint using one explicit-column copy that retains
  `retired_at`; do not execute seed inserts during the rebuild.
- Add a durable unique local-device claim keyed by the stable hardware token;
  registration claims it and worker retirement releases it transactionally.
- Add mutually exclusive CLI/DSL fields and fail-loud validation.

Red tests:

- HEVC NVENC accepts CQ and `p1..p7`; rejects CRF, CQ zero/out of range, unknown
  preset/tune/profile/level/pixel format, and NVIDIA decode with software
  encode.
- `av1_nvenc` is rejected as an unsupported profile encoder in this slice.
- Software fixtures serialize exactly as before and reject CQ.
- Migration snapshots every pre-0029 row's old columns, including `retired_at`,
  then preserves IDs, names, values, retirement markers, and row count exactly.
- A retired seed remains retired after migration, and migration neither
  conflicts with nor resurrects copied seed rows.
- CLI rejects missing or conflicting quality flags.

Verification:

```text
cargo test -p voom-core
cargo test -p voom-policy
cargo test -p voom-store video_profile
cargo test -p voom-cli profile
```

Commit boundary: typed NVIDIA profile and registry contract.

## Step 2 — Worker capability, UUID isolation, and policy preflight

Files:

- `crates/voom-worker-protocol/src/` typed accelerator capability/assignment
- `crates/voom-ffmpeg-worker/src/{preflight,main}.rs` and sibling tests
- `crates/voom-control-plane/src/local_worker.rs` and tests
- `crates/voom-control-plane/src/cases/policy/tool_preflight.rs` and tests
- `crates/voom-cli/src/{cli.rs,commands/execution/worker.rs}` and tests

Behavior:

- Accept a GPU UUID, put the Linux run-local supervisor and descendants in a
  dedicated process group, claim the UUID with the supervisor's PID/start
  identity/process-group ID and boot ID, set `CUDA_VISIBLE_DEVICES`, and use
  CUDA-visible device zero for every probe.
- Query exact NVIDIA identity, prove the FFmpeg PID-to-UUID binding,
  smoke-probe HEVC encode and per-device decoders, and run the declared HEVC
  concurrency probe alongside external encoder sessions.
- Derive the five-minute supervisor deadline from 18 maximum sequential
  deadline-bearing stages, their 15-second deadline, and 30 seconds of
  coordination allowance. Preserve the last in-flight probe name on timeout.
- Record typed `transcode_video` hardware capability and device session grant.
- Extend the existing pre-job tool check with requirements derived from
  resolved profiles, without promoting exact source codecs to run-level gates.

Red tests:

- Missing/malformed UUID, wrong PID-to-UUID readback, malformed `nvidia-smi`,
  build-listed but unusable encoder, permission failure, and insufficient
  declared concurrency all fail with context.
- The identity encode remains resident beyond its two-second poll window;
  successful early exit is retried as inconclusive, while an observed wrong UUID
  fails identity immediately.
- Each failed CUVID smoke probe is omitted from the descriptor with diagnostics;
  zero usable decoders blocks hardware-decode policy preflight but does not
  block software-decode HEVC NVENC.
- Session declarations outside 1..=16, probe timeout, child cleanup, and
  duplicate UUID claims fail visibly.
- Endpoint-unreachable but process-live owners cannot lose their claims; a dead
  owner transfers only after its process group is empty, with TERM/KILL and PID
  reuse paths covered. A boot-ID change clears the stale claim without
  signalling a numeric ID from the prior boot.
- External encoder sessions do not block readiness when every VOOM concurrency
  probe succeeds; a failed probe reports external contention separately from a
  claim-owned orphan and an invalid declaration.
- Empty, malformed, and `Not Supported` encoder-session enumeration preserves
  diagnostic uncertainty without blocking a successful capacity probe.
- At max sessions 16, the readiness deadline is strictly greater than the
  summed sequential stage budget; a readiness timeout names the active probe.
- Software run-local retains empty hardware and limit one.
- NVIDIA readiness persists stable UUID plus exact usable codec sets.
- A GPU-only worker fails policy preflight for a software profile; missing HEVC
  encoder, zero usable hardware decoders, and conflicting capacity declarations
  fail once before the job is opened.
- A mixed H.264/unsupported-codec input set with NVIDIA decode passes run
  preflight; planning blocks only the unsupported file.

Verification:

```text
cargo test -p voom-worker-protocol
cargo test -p voom-ffmpeg-worker preflight
cargo test -p voom-control-plane local_worker
cargo test -p voom-cli worker
```

Commit boundary: exact-device NVIDIA worker preflight and advertisement.

## Step 3 — Scheduler compatibility and atomic UUID capacity

Files:

- `crates/voom-store/src/repo/execution/{workers,leases}.rs` and tests
- `crates/voom-scheduler/src/lib.rs` and tests
- `crates/voom-control-plane/src/cases/policy/compliance.rs` and tests
- `crates/voom-control-plane/src/workflow/execution/executor/{config,mod,spawn}.rs`
- `crates/voom-cli/src/{cli.rs,commands/policy/compliance.rs}` and tests
- remote-acquire candidate assembly/tests
- planner/binding ticket payload tests

Behavior:

- Carry observed source codec into the ticket.
- Project typed accelerator descriptors into candidates.
- Filter software/NVIDIA requirements and pin deterministic reasons/ties.
- Revalidate the selected accelerator endpoint before acquiring its lease.
- Aggregate candidate sessions by stable hardware token and recheck
  compatibility/capacity inside lease acquisition.
- Carry the selected typed assignment through the runtime registry to dispatch.
- Refresh candidates and runtimes while a previously advertised accelerator is
  temporarily unavailable, without mutating the ticket or consuming an attempt.
- Add a monotonic unavailable clock keyed by hardware token. Start it on first
  absence and reset it only when that token regains an eligible,
  identity-verified descriptor.
- Add the positive `--accelerator-unavailable-timeout-seconds` execution option,
  default 900 seconds, reject values at or below NVIDIA's five-minute readiness
  deadline, and keep it independent of device-capacity waiting.

Red tests:

- Software excludes GPU workers; NVIDIA excludes software and wrong-codec
  workers.
- Equal compatible devices select deterministically.
- Two workers advertising one UUID share one capacity.
- A mid-run conflicting capacity declaration is recorded only against the
  affected transcode ticket, does not become a projection fatal, and allows
  unrelated tickets to continue in isolate failure mode.
- Concurrent acquisitions cannot exceed UUID capacity and leave rejected
  tickets/events unchanged.
- Never-advertised, temporarily unavailable, and capacity-full devices have
  distinct attempt/event behavior.
- The default unavailable timeout is strictly greater than the NVIDIA readiness
  deadline. A replacement that consumes nearly the full readiness budget still
  resumes dispatch.
- A concurrently active non-transcode ticket and unrelated dispatch progress do
  not reset or defer the unavailable token's expiry. Timeout stops new dispatch,
  drains active work, fails the job, and leaves the unavailable ticket ready
  with attempt zero.
- Reappearance resets only the matching token; two unavailable devices keep
  independent clocks.
- Unsupported NVIDIA-decode source codecs become per-file planning blocks, and
  a recognized codec missing from live descriptors fails only its ticket.

Verification:

```text
cargo test -p voom-scheduler
cargo test -p voom-store worker
cargo test -p voom-store lease
cargo test -p voom-control-plane executor
cargo test -p voom-control-plane remote
cargo test -p voom-cli compliance
```

Commit boundary: compatible-device scheduling and atomic capacity.

## Step 4 — Deterministic NVIDIA commands and result evidence

Files:

- `crates/voom-worker-protocol/src/operations/transcode_video.rs` and tests
- `crates/voom-ffmpeg-worker/src/{ffmpeg,handler}.rs` and tests
- `crates/voom-control-plane/src/transcode/{dispatch,events,mod}.rs` and tests
- `crates/voom-events/src/` payloads and tests

Behavior:

- Add expected source codec for NVIDIA decode.
- Require the scheduler-selected UUID assignment in hardware requests.
- Build one deterministic CPU-upload or CUDA-decode/filter/NVENC command.
- Return and validate exact device assignment.
- Persist optional hardware evidence without changing software facts.

Red tests:

- Golden HEVC encode-only and NVIDIA-decode command sequences for H.264, HEVC,
  and AV1 source codecs, with UUID visibility, CUDA device zero, and no `-gpu`
  argument. The HEVC/H.264 fixtures reproduce the prerequisite host argv.
- No implicit device, software fallback, duplicate `-vf`, or wrong CUDA
  transition.
- Malformed request/source mismatch and missing/wrong result assignment fail.
- Direct one-shot hardware dispatch fails with run-local guidance.
- Existing output-fact mismatch tests remain green.

Verification:

```text
cargo test -p voom-worker-protocol transcode_video
cargo test -p voom-ffmpeg-worker
cargo test -p voom-events transcode
cargo test -p voom-control-plane transcode
```

Commit boundary: NVIDIA FFmpeg execution and durable assignment evidence.

## Step 5 — NVIDIA host acceptance and operator documentation

Files:

- `docs/runbooks/operator-real-media-execution.md`
- NVIDIA acceptance script/test under the repository's existing real-media
  harness location

Behavior:

- Document one software worker plus one worker per GPU.
- Probe HEVC encode and each CUVID decoder independently on both installed GPUs.
- Run concurrent HEVC encode-only and NVIDIA-decode jobs on UUID-distinct
  workers, verify output facts, and assert each active FFmpeg PID appears under
  its configured UUID.
- Capture verbose NVIDIA-decode filter-graph logs and reject `hwdownload`,
  `hwupload`, or `auto_scale`; require CUDA input to `scale_cuda` and a CUDA
  frames context at NVENC.
- Execute AV1 hardware decode on each device that advertises `av1_cuvid`; do not
  advertise it on a device whose smoke decode fails.
- Document live-owner claims, orphan cleanup, external contention,
  conflicting-capacity, and unavailable-device remedies.

Expected environment:

```text
GPU 0: NVIDIA RTX A6000
GPU 1: Quadro RTX 4000
FFmpeg: 8.1.2 with hevc_nvenc and CUVID
```

Verification:

```text
<focused NVIDIA acceptance command added by this step>
just smoke
```

Commit boundary: NVIDIA real-media acceptance and runbook.

## Step 6 — Review, follow-ups, and shipping

Files:

- `docs/adr/0049-accelerator-devices-are-worker-resources.md`
- `docs/adr/README.md`
- `scripts/check-adr-index.sh` and its self-test
- `justfile` and `.pre-commit-config.yaml`

- After human design approval, change ADR 0049 from `proposed` to `accepted`;
  retain its issue #400 reference and the 0048/0049 index rows.
- Add `just check-adr-index`, wire it into `just ci` and prek, and prove its
  self-test fails for an unindexed ADR before passing for the repository.
- Update issue #400 to `status:in-review`.
- Run the adversarial review loop against `main...HEAD`, including security
  focus on process execution, device permissions, capability JSON, and
  transaction races.
- Run simplification review and apply only behavior-preserving reductions.
- Run `just ci`, create NVIDIA AV1 encode plus QSV/VAAPI/AMF/VideoToolbox
  follow-up issues with native links to #400, push the branch, create a PR
  closing #400, and drive CI to green/mergeable.
- Post `WORK:REVIEW`, transition to `status:awaiting-merge`, and post the final
  `WORK:TRAJECTORY`. Stop for merge handoff.

Commit boundary: review remediation and final documentation only when needed.
