---
status: accepted
date: 2026-07-29
deciders: [VOOM core]
---

# 0049 — Accelerator devices are worker resources

Issue: #400

## Context

Video transcode profiles and FFmpeg workers are software-only. A worker
capability can carry generic `hardware` and `extra` JSON, but scheduling uses
only the operation name and a worker-wide operation limit. Selecting an NVENC
profile without a typed device model could therefore dispatch to a CPU-only
worker, allow two workers to overcommit one GPU, or silently use FFmpeg's
first-available device.

Issue #400 proves the model with NVIDIA on the two-GPU acceptance host. QSV,
VAAPI, AMF, and VideoToolbox remain separate platform issues so their command
shapes and failure behavior are tested on matching systems.

The detailed design is recorded in the
[NVIDIA video acceleration design][nvidia-design].

[nvidia-design]: ../superpowers/specs/2026-07-29-issue-400-nvidia-video-acceleration-design.md

## Decision

1. A hardware video profile carries a typed backend requirement, not arbitrary
   FFmpeg arguments. Software profiles retain their current serialized fields
   and command shapes. HEVC NVENC profiles use `cq` rather than `crf`, a validated
   `p1..p7` preset, and a closed tune/profile/level/pixel-format vocabulary.
   Decode mode is a typed `software` or `nvidia` value; software is the omitted
   default. The NVIDIA slice permits NVIDIA decode only with HEVC NVENC.
   `av1_nvenc` is excluded until encode-capable acceptance hardware is available;
   independently probed `av1_cuvid` decode remains supported.

2. A GPU-configured local FFmpeg worker is bound to exactly one NVIDIA device.
   Configuration accepts a stable GPU UUID, not an ordinal. The supervisor
   starts the worker with `CUDA_VISIBLE_DEVICES=<GPU-uuid>`, and FFmpeg uses
   CUDA-visible device zero. Startup reads back the active FFmpeg PID's GPU UUID
   through `nvidia-smi` before advertising exact-device encoder and decoder
   usability. FFmpeg's global encoder/decoder list is necessary but not
   sufficient.

3. The operator declares a positive per-device session capacity, defaulting to
   one. The Linux run-local supervisor owns a dedicated process group, and the
   durable UUID claim records its boot ID, PID, process start identity,
   process-group ID, worker, and declaration. Endpoint silence never permits
   claim transfer. A process-live owner blocks replacement; after proven owner
   death, recovery terminates and verifies the old group before retiring it. A
   boot change proves the old processes are gone without signalling reused
   numeric IDs. The declaration is bounded to 1..=16 so startup cannot create
   unbounded probe processes.

   Device-global encoder idleness is not required. Startup proves the declaration
   with concurrent claim-owned smoke encodes alongside any external sessions.
   A successful probe permits readiness. A failed probe reports external
   contention separately from a VOOM orphan or an unsupported declaration when
   encoder-session enumeration is trustworthy. Empty, malformed, or unsupported
   enumeration remains diagnostic uncertainty and never blocks a successful
   capacity probe.

4. The `transcode_video` capability records a typed accelerator descriptor in
   `extra` and a stable `nvidia:<gpu-uuid>` token in `hardware`. The descriptor
   includes backend, UUID, device name, usable encoders, usable decoders, and
   tested session capacity. Other FFmpeg operations remain ordinary operation
   capabilities.

5. Before opening a job, policy preflight derives profile-level video
   requirements. A software transcode requires an unaccelerated FFmpeg worker.
   An NVIDIA transcode requires an identity-verified HEVC NVENC descriptor; a
   hardware-decode profile additionally requires at least one usable CUVID
   decoder. Exact source-decoder compatibility remains per-file. Unsupported
   source codecs become planner-blocked files, while a recognized codec missing
   from every live descriptor becomes a ticket-scoped `MissingCapability`.
   Dispatch repeats endpoint identity validation before acquiring an
   accelerator lease. Equal eligible loads retain deterministic worker-ID tie
   breaking.

6. Candidate capacity and lease acquisition count held `transcode_video`
   leases across every worker advertising the same stable hardware token.
   Lease acquisition re-parses the ticket's typed profile, rechecks exact
   compatibility, and rechecks UUID-wide capacity while holding SQLite's write
   lock. Process-local reservations remain advisory. Duplicate live workers for
   one GPU cannot multiply capacity. Preflight blocks conflicting capacity
   declarations before a new job. A conflict that appears during a run becomes
   a typed terminal rejection for only the affected transcode ticket; it never
   escapes candidate projection as a job-fatal repository error.

7. Scheduler selection returns the exact accelerator assignment as well as the
   worker. Dispatch carries the selected backend and UUID in the worker request,
   plus the expected source codec when NVIDIA decode is selected. The worker
   requires that assignment to match its own startup descriptor before FFmpeg
   execution. The worker result, transcode success event, and execution report
   echo the actual assignment. The control plane rejects a missing or
   mismatched assignment as a malformed worker result.

8. FFmpeg commands always select the configured device explicitly. Software
   decode plus NVENC uses the required CUDA upload/pixel-format transition.
   NVIDIA decode uses the matching CUVID decoder and keeps frames on the same
   CUDA device through optional `scale_cuda` and NVENC. Both paths take the
   device from their CUDA hardware-frames context; `-gpu` is not used. No
   failure path substitutes a software encoder or decoder.

9. Every external preflight process has a fixed deadline and is killed and
   reaped on timeout. NVIDIA run-local readiness is derived from 18 maximum
   sequential stages at 15 seconds plus 30 seconds of coordination allowance,
   for a five-minute bound. Capacity children run concurrently and count as one
   stage. The supervisor tracks and reports the last in-flight probe on timeout.
   Probe files live in a private temporary directory and are removed on every
   success or failure path.

10. If a matching accelerator disappears after run preflight, dispatch defers
    without mutating the ticket or consuming an attempt and refreshes workers at
    the existing capacity-retry interval. A replacement incarnation can resume
    the run. The executor owns an independent monotonic clock per unavailable
    hardware token, started by absence and reset only by an eligible,
    identity-verified descriptor for that token. Unrelated progress and active
    leases do not reset it.

    The operator-configurable timeout defaults to 15 minutes, must exceed the
    five-minute NVIDIA readiness deadline, and is separate from the one-minute
    capacity clock. It bounds operator or service-manager reaction because VOOM
    does not automatically restart a run-local supervisor. On expiry, the job
    fails while the ticket remains ready. A requirement that has never had a
    matching durable descriptor retains the ordinary attempt-consuming
    `NO_ELIGIBLE_WORKER` backstop.

## Consequences

- Existing software profile JSON and FFmpeg arguments remain byte-for-byte
  stable. Profile storage gains nullable `crf`, nullable `cq`, and a typed
  decode-mode column; descriptor validation requires exactly the field
  appropriate to the encoder.
- Hardware selection is based on stable physical identity and is independent of
  NVML and CUDA enumeration order.
- A configured capacity is deliberately conservative and reproducible. Raising
  it requires an operator choice and a successful concurrent startup probe.
- External encoder sessions do not categorically block startup and are never
  counted as VOOM leases. Operators must leave headroom for co-tenants; later
  external contention can still produce an ordinary retriable worker failure.
- A temporarily absent accelerator can hold a run for the configured recovery
  window. The 15-minute default assumes an external service manager or operator
  starts the replacement; VOOM supplies no automatic restart loop.
- Automatic claim recovery is Linux-only in this slice. It never steals from a
  process-live supervisor and never signals a reused process identity or an
  external encoder process.
- GPU-configured workers do not execute software video profiles. Operators who
  need both modes run a software FFmpeg worker alongside one worker per GPU.
- The capability JSON establishes the backend-neutral seam used by later
  accelerator issues, while this ADR defines only NVIDIA vocabulary and
  commands.

## Considered and rejected alternatives

- **Trust `ffmpeg -encoders` and `-decoders`.** Rejected because the installed
  build advertises `av1_nvenc` on the acceptance host while both installed GPUs
  reject it.
- **Ship AV1 NVENC from string-only tests.** Rejected because neither acceptance
  GPU can encode AV1. AV1 encode is deferred to a hardware-backed follow-up;
  per-device AV1 decode remains probe-gated.
- **Let FFmpeg choose `-gpu any`.** Rejected because scheduler assignment,
   reproducibility, and cross-worker isolation would be unverifiable.
- **Resolve a device through an NVML ordinal.** Rejected because NVML and CUDA
  can enumerate the same host differently. UUID visibility plus CUDA device
  zero has one namespace, and PID-to-UUID readback makes the binding falsifiable.
- **Use worker-wide `max_parallel` as device capacity.** Rejected because two
  worker processes bound to one UUID would each receive the full limit.
- **Create one issue implementing every backend.** Rejected because untested
  platform command shapes would become phantom support.
- **Require device-global encoder idleness before startup.** Rejected because an
  unrelated co-tenant session would make VOOM wholly unavailable even when the
  declared VOOM concurrency still fits.
- **Automatically discover the maximum NVENC session count.** Rejected because
  driver/product limits are not exposed as an authoritative portable query and
  can differ from currently usable capacity.
- **Support NVIDIA decode with software encoders in this slice.** Rejected
  because it adds a separate download/format path without being needed to
  prove NVENC encode-only and NVIDIA zero-copy decode/encode.
- **Persist GPU ordinals as identity.** Rejected because ordinals are host-local
  and can change; the NVIDIA UUID is stable.
