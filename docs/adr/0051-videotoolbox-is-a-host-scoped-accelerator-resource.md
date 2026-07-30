---
status: accepted
date: 2026-07-29
deciders: [VOOM core]
---

# 0051 — VideoToolbox is a host-scoped accelerator resource

Issue: #411

## Context

ADR 0049 models accelerator devices as worker resources and proves that model
with selectable NVIDIA GPU UUIDs. Apple VideoToolbox has different identity and
ownership semantics:

- Apple silicon exposes one host-integrated media subsystem rather than an
  operator-selectable GPU UUID;
- FFmpeg can require hardware encoding and preserve VideoToolbox frames, but
  encoder and decoder list entries alone do not prove usability;
- macOS provides a boot-session identity, but this unsafe-code-free Rust
  workspace has no precise standard-library process-start identity;
- session capacity is observable through concurrent execution, not a portable
  authoritative maximum query.

Treating VideoToolbox as NVIDIA with renamed strings would either persist a
false device identity, permit ambiguous claim stealing, or advertise unexecuted
codec/format support.

The detailed design and host evidence are in the
[VideoToolbox video acceleration design][design].

[design]: ../superpowers/specs/2026-07-29-issue-411-videotoolbox-video-acceleration-design.md

## Decision

1. VideoToolbox is one host-scoped accelerator resource on the supported
   Apple-silicon Mac. Its stable resource ID is the lowercase SHA-256 digest of
   normalized `IOPlatformUUID`. The raw UUID is discarded and never persisted
   or logged. Model, chip, macOS version/build, and proven media capabilities
   remain non-secret evidence.

2. Accelerator descriptors become a strict tagged enum with NVIDIA and
   VideoToolbox content structs. A migration adds the `nvidia` tag to existing
   durable capability JSON so there is one post-migration format. Requirements
   and assignments gain matching VideoToolbox variants.

3. VideoToolbox profiles are typed H.264 or HEVC bitrate profiles. They require
   positive `bitrate_kbps`, use the single `default` preset, and accept only
   the profile/level/pixel-format vocabulary executed on the acceptance Mac.
   Every VideoToolbox profile requires a complete, paired
   profile/level/pixel-format tuple so output bit depth never depends on an
   FFmpeg default. VideoToolbox decode is explicit and is permitted only with
   a VideoToolbox encoder.

4. Decoder capability is a source codec and pixel-format pair. H.264, HEVC,
   and AV1 combinations are advertised only after real hardware-decode to
   hardware-encode smoke pipelines succeed. Input and output bit depth must
   match so zero-copy decode never hides a system-memory conversion.

5. A local worker is configured explicitly with `--videotoolbox` and a
   declared capacity in `1..=16`. The supervisor and worker independently
   derive the resource digest. The unique durable claim is acquired before the
   worker runs probes, and readiness requires concurrent execution of the full
   declaration for every advertised encoder and decoder-format path. The one
   token-wide limit is therefore the minimum proven across all supported paths,
   not a value inferred from one representative codec.

6. The accelerator claim schema is backend-neutral. Existing NVIDIA process
   ticks become a prefixed string identity. VideoToolbox stores no process-start
   identity. On macOS, a boot change permits retirement; on the same boot,
   recovery requires both the recorded PID and process group to be absent.
   A live or ambiguous group is never signalled or stolen.

7. Scheduling and lease capacity remain stable-token-wide. Compatibility
   requires the exact backend, encoder, and optional decoder codec/format.
   Selection carries the exact resource assignment through the request,
   result, event, and report.

8. Software decode uses one explicit CPU filter graph ending in `nv12` or
   `p010le`. VideoToolbox decode requires
   `-hwaccel videotoolbox -hwaccel_output_format videotoolbox_vld` and uses
   `scale_vt` only when downscaling. Every VideoToolbox encoder command includes
   `-allow_sw 0`. No failure path retries in software.

9. This decision supports Apple silicon only. ProRes, Intel Mac, multi-GPU
   selection, theoretical capacity discovery, hardware decode into a software
   encoder, and remote VideoToolbox configuration require separate
   hardware-backed decisions.

## Consequences

- H.264 becomes a supported video target and profile storage gains nullable
  `bitrate_kbps`.
- Capability history gains an explicit backend tag; migration tests must prove
  existing NVIDIA evidence is retained.
- Assignment evidence uses a generic resource ID for VideoToolbox while
  retaining the existing NVIDIA UUID field.
- Same-boot macOS recovery can require manual cleanup after an abrupt crash.
  This availability cost is preferred to signalling or stealing an ambiguously
  reused process group.
- A declared capacity is conservative and reproducible for the tested host.
  Raising it requires a successful concurrent startup probe.
- Hardware-required encoder flags and hardware-frame output format make
  fallback failure visible rather than silently changing performance or
  quality.
- An Intel Mac cannot start a VideoToolbox worker until a separate acceptance
  issue defines its GPU identity and command evidence.

## Considered and rejected alternatives

- **Persist raw `IOPlatformUUID`.** Rejected because the stable identity can be
  matched using a digest without exposing the host identifier in durable
  records or logs.
- **Use a fixed `videotoolbox:local` token.** Rejected because moving a database
  between Macs would falsely identify different hardware as the same resource.
- **Use model and chip name as identity.** Rejected because two identical Macs
  would collide.
- **Treat the integrated GPU as a selectable device.** Rejected because the
  accepted FFmpeg commands expose no device-selection contract on Apple
  silicon.
- **Reuse Linux process-start recovery on macOS.** Rejected because the
  workspace forbids unsafe Rust and a coarse timestamp could authorize
  signalling a reused PID. Conservative refusal is safe and actionable.
- **Trust FFmpeg inventory strings.** Rejected because they show build
  availability, not permission, hardware availability, format support, or
  usable concurrency.
- **Allow `-allow_sw 1`.** Rejected because it silently changes a hardware
  profile into software execution.
- **Automatically download hardware frames for format conversion.** Rejected
  because it violates the explicit no-fallback/no-hidden-transition contract.
- **Add ProRes because FFmpeg lists it.** Rejected because the issue needs the
  existing transcode target model, and no ProRes profile/container/evidence
  contract was requested.
- **Advertise Intel Mac support from Apple-silicon results.** Rejected because
  device identity, GPU selection, codecs, and session behavior differ.
