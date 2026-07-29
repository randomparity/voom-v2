---
status: accepted
date: 2026-07-29
deciders: [VOOM core]
---

# 0050 — VAAPI device identity is the PCI address, and capability is probe-proven

Issue: #409

## Context

ADR 0049 established the typed accelerator-resource model and proved it with
NVIDIA, deferring VAAPI so its command shapes and failure behavior could be
tested on matching hardware. That hardware is now available: an AMD Strix Halo
APU (Radeon 8060S) on Fedora 44, Mesa 26.1.5 `radeonsi`, FFmpeg 8.1.2.

VAAPI does not supply the primitives ADR 0049 leaned on for NVIDIA. It has no
device UUID, no `nvidia-smi` equivalent for identity readback, and no encoder-session
enumeration. Two further properties fall out of the acceptance probe:

1. **Usable codecs are a property of the loaded driver build, and are invisible
   from the device.** On the acceptance host, stock `mesa-dri-drivers` advertises
   AV1 encode only, while `mesa-va-drivers-freeworld` on the same GPU, same render
   node, and same FFmpeg additionally advertises H.264 and HEVC encode. Installing
   the freeworld package places `/usr/lib64/dri-freeworld/` on the global library
   path and makes it the system-wide default, so the swap is not visible in the
   render-node path, in FFmpeg's encoder list, or in any per-process configuration.
   The same `vainfo` binary reports different capability depending only on which
   driver build got loaded.

2. **Render-node numbers are not identity.** `/dev/dri/renderD128` is assigned by
   enumeration order and can renumber; the PCI address behind it cannot.

The design detail is recorded in the [VAAPI video acceleration design][vaapi-design].

[vaapi-design]: ../superpowers/specs/2026-07-29-issue-409-vaapi-video-acceleration-design.md

## Decision

1. **A VAAPI worker is configured with a PCI address, never a render-node path or
   ordinal.** Startup resolves the address through
   `/dev/dri/by-path/pci-<addr>-render` and reads the resolved node's PCI address
   back before advertising anything, making the binding falsifiable. The hardware
   token is `vaapi:pci-<addr>`. This is the direct analogue of ADR 0049 §2's
   UUID-not-ordinal rule and satisfies issue #409's stability requirement rather
   than its detect-and-reject fallback.

2. **Advertised capability is proven by executing a claim-owned smoke encode on the
   bound device, per codec.** FFmpeg's encoder list and `vainfo`'s entrypoint list
   are necessary but never sufficient, because neither distinguishes the two driver
   builds above. A codec that has not encoded on this device in this process's
   driver environment is not advertised.

3. **The first slice is `hevc_vaapi` encode — Main and Main10 — plus typed VAAPI
   decode for `h264`, `hevc`, and `av1`.** `h264_vaapi` and `av1_vaapi` encode are
   proven on the acceptance host but deferred to follow-up issues, keeping this
   slice's conformance surface to the codec that matches VOOM's existing
   `default-hevc` profile.

4. **VAAPI profiles carry a quality-parameter domain and no preset.** `qp` is
   constrained to `1..=52`: FFmpeg accepts `0..52` and rejects 53, and 0 is the
   default meaning auto, so it is excluded from the operator vocabulary. `hevc_vaapi`
   exposes no `-preset` and no `-compression_level`, so `preset` becomes nullable and
   a migration `CHECK` forbids it for VAAPI while keeping it mandatory for every
   other encoder. `codec_level` is rejected for VAAPI: `-level` is an integer
   `general_level_idc` whose auto-derivation is correct, and a half-supported level
   vocabulary would be phantom support.

5. **Rate control is always explicit.** Generated commands pass `-rc_mode CQP -qp N`
   rather than relying on `auto`, so rate-control behavior cannot drift with an
   FFmpeg or driver default. Frame transfers are explicit in both directions:
   `format=nv12,hwupload` (or `format=p010` for Main10) for a software-decoded
   source, and a bare hardware-frame path with no filter for VAAPI-decoded input.
   There is no software-encoder fallback.

6. **Declared concurrency is operator-supplied, bounded `1..=16`, and proven by
   concurrent smoke encodes.** VAAPI exposes no session enumeration, so ADR 0049 §3's
   separation of external contention from a VOOM orphan has no VAAPI counterpart:
   a failed capacity probe is always reported as diagnostic uncertainty. This is a
   permanent property of the API, not a gap to be closed later.

## Consequences

- Existing software and NVIDIA profile JSON and FFmpeg arguments remain
  byte-for-byte stable. Profile storage gains a nullable `qp`, a widened
  `decode_backend` vocabulary, and a `preset` column that is nullable at the schema
  level but still mandatory per-encoder via `CHECK`.
- H.264 and HEVC VAAPI encode require a driver build carrying those codecs, which on
  Fedora means the RPM Fusion `mesa-va-drivers-freeworld` package. AV1 encode and all
  hardware decode work on the stock driver. Operators who cannot install that package
  get an actionable preflight failure rather than silent software encoding.
- Because capability tracks the loaded driver build rather than the hardware, a host
  driver change can move a worker's advertised codecs without any VOOM configuration
  change. The startup probe is the only thing that detects this, so it runs on every
  start and is not cached across restarts.
- A failed VAAPI capacity probe cannot attribute the cause. Operators diagnosing
  contention on a shared GPU get less signal than on NVIDIA, by construction.
- `LocalWorkerBound.accelerator` becomes a tagged enum rather than an
  NVIDIA-specific optional struct, so the worker-protocol change is not additive and
  is coordinated binary-before-DB per ADR 0013.
- The acceptance host has a single render node, so per-device capacity and
  no-cross-device assignment are covered by scheduler unit tests rather than a
  real-media two-device run. ADR 0049's two-GPU concurrency evidence has no
  counterpart here.
- `VideoDecodeMode::is_nvidia()` stops being defined as "not software". Adding a
  third backend makes that definition wrong, and it gates profile validation.

## Considered and rejected alternatives

- **Trust `vainfo` entrypoints, or FFmpeg's encoder list, as proof of support.**
  Rejected because both report HEVC encode as available or unavailable on the same
  GPU depending only on which driver build is loaded, and the loaded build is not
  visible from either.
- **Configure the worker with a render-node path.** Rejected because render-node
  numbers are enumeration-order artifacts. The PCI address is the stable anchor and
  resolves to exactly one node.
- **Pin the driver build per worker with `LIBVA_DRIVERS_PATH`.** Rejected as a
  capability guarantee: the acceptance probe showed the freeworld package becomes the
  global default with the variable unset, so a worker that merely sets a path has
  still not established what that path's driver can do. Executing a probe encode is
  the only falsifiable check. The variable remains useful to operators for pinning,
  and is out of scope as a correctness mechanism.
- **Give VAAPI a synthetic one-value preset so `preset` stays `NOT NULL`.** Rejected
  because it puts an operator-facing knob in the vocabulary that maps to no FFmpeg
  flag.
- **Map the profile's preset onto `-async_depth`.** Rejected because `async_depth` is
  processing parallelism, not a speed/quality tradeoff, and it would overlap the
  per-device capacity model in ADR 0049 §3.
- **Include `h264_vaapi` and `av1_vaapi` encode in this slice.** Rejected as scope:
  all three encoders are proven, but each additional encoder multiplies the pinned
  command shapes and preflight permutations without proving anything new about the
  VAAPI device model. They are follow-up issues with hardware already in hand.
- **Expose `codec_level` for VAAPI.** Rejected because `-level` takes an integer
  `general_level_idc` and FFmpeg derives a correct value automatically; a partial
  name-to-integer level table would be phantom support.
- **Expose `rc_mode` as a profile field.** Rejected as speculative. CQP is the mode
  this slice proves; other modes can be added when an operator needs one.
- **Reuse the NVIDIA `max_sessions` probe strategy for capacity.** Rejected because
  VAAPI has no session-count query at all, so the declaration cannot be
  cross-checked against an authoritative number the way NVML permits.
- **Treat the absent session enumeration as a temporary gap.** Rejected because
  VAAPI specifies no such query; recording it as permanent uncertainty is honest and
  stops a later issue from chasing it.
